use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Output,
};

use axum::{
    Json, Router,
    extract::{self, RawQuery, State},
    http::{
        self, HeaderMap, HeaderValue, Method, Response, StatusCode,
        header::{self, CONTENT_TYPE},
    },
    response::IntoResponse,
    routing::{delete, get, post},
};
use deadpool_diesel::{InteractError, Pool, PoolError};
use diesel::{expression::is_aggregate::No, prelude::*, result};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use dotenvy::dotenv;
use sha2::{Digest, Sha256};
use tokio::{fs, process::Command, sync::mpsc::Sender};
use tokio_util::io::ReaderStream;
use tower_http::{
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod models;
mod schema;
use models::*;

use uuid::Uuid;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/");

#[derive(serde::Deserialize, Clone)]
struct FfprobeOutput {
    pub streams: Vec<FfprobeStream>,
}

#[derive(serde::Deserialize, Clone)]
struct FfprobeStream {
    codec_name: String,
    index: u32,
    avg_frame_rate: String,
    codec_type: String,
}

fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;

    let _ = std::io::copy(&mut file, &mut hasher)?;
    let hash_bytes = hasher.finalize();

    Ok(hex::encode(hash_bytes))
}

// TODO: improve error management
#[derive(Debug)]
enum CustomErrors {
    Diesel(diesel::result::Error),
    Deadpool(InteractError),
    DeadpoolPool(PoolError),
    Custom(String),
}

async fn process_link(
    pool: &Pool<deadpool_diesel::Manager<SqliteConnection>>,
    link: &Link,
    download_folder: &String,
    transcode_folder: &String,
) -> Result<(), CustomErrors> {
    use crate::schema::links;

    let download_path = PathBuf::from(download_folder).join(link.id.as_str());
    let tmp_download_path = PathBuf::from(download_folder)
        .join("tmp")
        .join(link.id.as_str());
    let transcode_path = PathBuf::from(transcode_folder).join(link.id.as_str());
    let conn = pool.get().await.map_err(CustomErrors::DeadpoolPool)?;

    let url: String = link.url.clone();
    let id: String = link.id.clone();

    info!("processing {}", &url);
    let result = conn
        .interact(|conn| {
            links::table
                .filter(links::url.eq(url))
                .filter(links::id.ne(id))
                .load::<Link>(conn)
        })
        .await
        .map_err(CustomErrors::Deadpool)?
        .map_err(CustomErrors::Diesel)?;
    if !result.is_empty() {
        let other_link = result.first().unwrap();
        debug!("found similar link: {:?}", other_link);
        let other_transcode_path = PathBuf::from(transcode_folder).join(other_link.id.as_str());
        Command::new("cp")
            .arg("--reflink=always")
            .arg(other_transcode_path)
            .arg(&transcode_path);
        let hash = other_link.original_hash.clone();
        let id = link.id.clone();
        conn.interact(|conn| {
            diesel::update(links::dsl::links)
                .filter(links::dsl::id.eq(id))
                .set(links::dsl::original_hash.eq(hash))
                .execute(conn)
        })
        .await
        .map_err(CustomErrors::Deadpool)?
        .map_err(CustomErrors::Diesel)?;

        return Ok(());
    }

    let mut cmd = Command::new("yt-dlp");
    let download_path_str = download_path
        .to_str()
        .ok_or(CustomErrors::Custom("cannot convert path".to_string()))?;
    cmd.arg("-o")
        .arg(&tmp_download_path)
        .arg("--max-downloads")
        .arg("1")
        .arg("-f")
        .arg("ba*") // best audio
        .arg("-S")
        .arg("+res:720") // 720p max
        .arg("--exec")
        .arg(format!("mv {{}} {download_path_str}"))
        .arg(link.url.as_str());

    debug!("yt-dlp command {:?}", cmd);

    let _ = cmd
        .output()
        .await
        .map_err(|e| CustomErrors::Custom(e.to_string()));

    if !download_path.exists() {
        return Err(CustomErrors::Custom("downloaded file not found".into()));
    }

    debug!("hashig file");
    let hash = hash_file(&download_path).map_err(|e| CustomErrors::Custom(e.to_string()))?;
    let hash_insert = Some(hash.clone());

    let id = link.id.clone();

    let result = conn
        .interact(|conn| {
            links::table
                .filter(links::original_hash.eq(hash))
                .load::<Link>(conn)
        })
        .await
        .map_err(CustomErrors::Deadpool)?
        .map_err(CustomErrors::Diesel)?;
    conn.interact(|conn| {
        diesel::update(links::dsl::links)
            .filter(links::dsl::id.eq(id))
            .set(links::dsl::original_hash.eq(hash_insert))
            .execute(conn)
    })
    .await
    .map_err(CustomErrors::Deadpool)?
    .map_err(CustomErrors::Diesel)?;

    debug!("transcoding {}", &transcode_path.to_string_lossy());

    if !result.is_empty() {
        let other_link = result.first().unwrap();
        let other_transcode_path = PathBuf::from(transcode_folder).join(other_link.id.as_str());
        Command::new("cp")
            .arg("--reflink=always")
            .arg(other_transcode_path)
            .arg(transcode_path);
    } else {
        let Output { stdout, .. } = Command::new("ffprobe")
            .arg("-v")
            .arg("quiet")
            .arg("-print_format")
            .arg("json")
            .arg("-show_format")
            .arg("-show_streams")
            .arg(&download_path)
            .output()
            .await
            .map_err(|e| CustomErrors::Custom(e.to_string()))?;

        let parsed: FfprobeOutput =
            serde_json::from_slice(&stdout).map_err(|e| CustomErrors::Custom(e.to_string()))?;

        let mut video_stream = None;
        let mut audio_stream = None;

        for stream in parsed.streams {
            if stream.codec_type == "video" && stream.avg_frame_rate.as_str() != "0/0" {
                video_stream.replace(stream.index);
            }
            if stream.codec_type == "audio" {
                audio_stream.replace(stream.index);
            }
        }

        let mut command = Command::new("ffmpeg");

        command.arg("-i").arg(&download_path);

        let mut complex_filter = false;

        if video_stream.is_none()
        // && let Some(audio_stream) = audio_stream
        {
            let mut image_path = PathBuf::from(&download_path);
            image_path.add_extension("png");

            let out = Command::new("ffmpeg")
                .arg("-i")
                .arg(&download_path)
                .arg(&image_path)
                .output()
                .await
                .map_err(|e| CustomErrors::Custom(e.to_string()))?;

            debug!("thumbnail extraction: {:?}", out);

            if image_path.exists() {
                command
                    .arg("-loop")
                    .arg("1")
                    .arg("-framerate")
                    .arg("10")
                    .arg("-i")
                    .arg(&image_path)
                    .arg("-map")
                    .arg("1:v:0") // put video first
                    .arg("-map")
                    .arg("0:a:0")
                    .arg("-tune")
                    .arg("stillimage")
                    .arg("-shortest")
                    .arg("-shortest_buf_duration")
                    .arg("60");
                // .arg("-r")
                // .arg("5");
            } else {
                command
                    .arg("-filter_complex")
                    .arg("[0:a]a3dscope=s=848x480:r=24");
                complex_filter = true;
            }
            fs::remove_file(image_path)
                .await
                .map_err(|e| CustomErrors::Custom(e.to_string()))?;
        }

        if !complex_filter {
            command.arg("-vf").arg("scale=-2:720:flags=lanczos");
        }

        command
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-movflags")
            .arg("faststart")
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("veryfast")
            .arg("-crf")
            .arg("28")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("280k")
            .arg("-f")
            .arg("mp4")
            .arg(&transcode_path);

        debug!("ffmpeg command: {:?}", command);

        let result = command
            .output()
            .await
            .map_err(|e| CustomErrors::Custom(e.to_string()))?;

        debug!("ffmpeg result: {:?}", result);
    }

    fs::remove_file(download_path)
        .await
        .map_err(|e| CustomErrors::Custom(e.to_string()))?;

    Ok(())
}

async fn tasks_manager(
    pool: Pool<deadpool_diesel::Manager<SqliteConnection>>,
    control_rx: &mut tokio::sync::mpsc::Receiver<Link>,
    download_folder: String,
    transcode_folder: String,
) {
    while let Some(link) = control_rx.recv().await {
        debug!("received {:?}", link);
        let result = process_link(&pool, &link, &download_folder, &transcode_folder).await;
        debug!("processing result: {:?}", result);

        match pool.get().await {
            Ok(conn) => {
                let id = link.id;
                let url = link.url;
                if let Err(e) = conn
                    .interact(move |conn| {
                        let res = if let Err(e) = result {
                            error!("Failed to process link {},{:?}", url, e);
                            diesel::update(schema::links::dsl::links)
                                .filter(schema::links::dsl::id.eq(id))
                                .set(schema::links::dsl::error.eq(format!("{:?}", e)))
                                .execute(conn)
                        } else {
                            diesel::update(schema::links::dsl::links)
                                .filter(schema::links::dsl::id.eq(id))
                                .set(schema::links::dsl::finished.eq(true))
                                .execute(conn)
                        };

                        if let Err(e) = res {
                            error!("Error updating database: {}", e);
                        }
                    })
                    .await
                {
                    error!("Error updating database: {}", e);
                };
            }
            Err(e) => {
                error!("Failed to open conn: {}", e);
            }
        };
    }
}

#[derive(Clone)]
struct AppState {
    pool: Pool<deadpool_diesel::Manager<SqliteConnection>>,
    control_tx: Sender<Link>,
    transcode_folder: String,
    download_folder: String,
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("{}=debug", env!("CARGO_CRATE_NAME")).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let db_url = std::env::var("DATABASE_URL").unwrap();
    let download_folder = std::env::var("DOWNLOAD_FOLDER").unwrap_or("./download".to_string());
    let transcode_folder = std::env::var("TRANSCODE_FOLDER").unwrap_or("./transcode".to_string());
    let frontend_folder = std::env::var("FRONTEND_FOLDER").unwrap_or("./frontend".to_string());

    fs::create_dir_all(&transcode_folder).await.unwrap();

    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel(128);

    // set up connection pool
    let manager = deadpool_diesel::sqlite::Manager::new(db_url, deadpool_diesel::Runtime::Tokio1);
    let pool: deadpool_diesel::Pool<deadpool_diesel::Manager<SqliteConnection>> =
        deadpool_diesel::sqlite::Pool::builder(manager)
            .build()
            .unwrap();

    // run the migrations on server startup
    {
        let conn = pool.get().await.unwrap();
        conn.interact(|conn| conn.run_pending_migrations(MIGRATIONS).map(|_| ()))
            .await
            .unwrap()
            .unwrap();
    }
    let task_pool = pool.clone();
    let transcode_folder_clone = transcode_folder.clone();
    let download_folder_clone = download_folder.clone();
    tokio::spawn(async move {
        tasks_manager(
            task_pool,
            &mut control_rx,
            download_folder_clone,
            transcode_folder_clone,
        )
        .await
    });

    let cors = CorsLayer::new()
        .allow_headers([CONTENT_TYPE])
        .allow_methods([Method::POST, Method::GET, Method::DELETE])
        .allow_origin(Any);

    let frondend_dir = ServeDir::new(&frontend_folder)
        .not_found_service(ServeFile::new(format!("{}/200.html", &frontend_folder)));

    let transcode_dir = ServeDir::new(transcode_folder.clone());

    // build our application with some routes

    let api = Router::new()
        .route("/links", get(list_links))
        .route("/link", post(create_link))
        .route("/link/{id}", get(show_link))
        .route("/link/{id}", delete(delete_link))
        .route("/direct/{*url}", get(direct_request))
        .with_state(AppState {
            pool,
            control_tx,
            transcode_folder,
            download_folder,
        });
    let app = Router::new()
        .nest("/api", api)
        .nest_service("/transcode", transcode_dir)
        .fallback_service(frondend_dir)
        .layer(cors);

    // run it with hyper
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::debug!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn list_links(
    State(app_state): State<AppState>,
) -> Result<Json<Vec<Link>>, (StatusCode, String)> {
    use schema::links::dsl::*;

    let conn = app_state.pool.get().await.map_err(internal_error)?;
    let res = conn
        .interact(|conn| links.select(Link::as_select()).load(conn))
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;

    Ok(Json(res))
}

async fn show_link(
    State(app_state): State<AppState>,
    extract::Path(link_id): extract::Path<String>,
) -> Result<Json<Link>, (StatusCode, String)> {
    use schema::links::dsl::*;

    let conn = app_state.pool.get().await.map_err(internal_error)?;

    let res = conn
        .interact(|conn| {
            links
                .select(Link::as_select())
                .filter(id.eq(link_id))
                .load(conn)
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;

    if let Some(l) = res.first() {
        Ok(Json(l.clone()))
    } else {
        Err((StatusCode::NOT_FOUND, "File not found".to_string()))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NewLink {
    pub url: String,
}

async fn create_link(
    State(app_state): State<AppState>,
    Json(new_link): Json<NewLink>,
) -> Result<Json<Link>, (StatusCode, String)> {
    use crate::schema::links;

    let conn = app_state.pool.get().await.map_err(internal_error)?;

    let link = Link {
        url: new_link.url,
        id: hex::encode(Uuid::new_v4().as_bytes()),
        ..Default::default()
    };
    let res = conn
        .interact(|conn| {
            diesel::insert_into(links::table)
                .values(link)
                .returning(Link::as_returning())
                .get_result(conn)
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;

    app_state
        .control_tx
        .send(res.clone())
        .await
        .map_err(internal_error)?;
    Ok(Json(res))
}

#[axum::debug_handler]
async fn direct_request(
    State(app_state): State<AppState>,
    extract::Path(root_url): extract::Path<String>,
    RawQuery(query): RawQuery,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // FIXME: find a way to have axum not interpret query parameters
    let url = match query {
        Some(q) => format!("{}?{}", root_url, q),
        None => root_url,
    };

    info!("direct request on {}", url);
    use crate::schema::links;

    let conn = app_state.pool.get().await.map_err(internal_error)?;
    let url_clone = url.clone();
    let result = conn
        .interact(|conn| {
            links::table
                .filter(links::url.eq(url_clone))
                .load::<Link>(conn)
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;
    let link = if let Some(link) = result.first() {
        link.clone()
    } else {
        let link = Link {
            url,
            id: hex::encode(Uuid::new_v4().as_bytes()),
            ..Default::default()
        };

        let link_insert = link.clone();
        let _ = conn
            .interact(|conn| {
                diesel::insert_into(links::table)
                    .values(link_insert)
                    .returning(Link::as_returning())
                    .get_result(conn)
            })
            .await
            .map_err(internal_error)?
            .map_err(internal_error)?;
        link
    };

    let res = process_link(
        &app_state.pool,
        &link,
        &app_state.download_folder,
        &app_state.transcode_folder,
    )
    .await;

    match res {
        Ok(()) => {}
        Err(e) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{:?}", e)));
        }
    }

    let transcoded_path = PathBuf::from(app_state.transcode_folder).join(link.id);

    let file = match tokio::fs::File::open(transcoded_path).await {
        Ok(file) => file,
        Err(err) => return Err((StatusCode::NOT_FOUND, format!("File not found: {}", err))),
    };
    // // convert the `AsyncRead` into a `Stream`
    let stream = ReaderStream::new(file);
    // // convert the `Stream` into an `axum::body::HttpBody`
    let body = axum::body::Body::from_stream(stream);

    // let file = ServeFile::new(transcoded_path).oneshot();

    let mut headers = HeaderMap::new();
    headers.append(
        http::header::CONTENT_TYPE,
        http::header::HeaderValue::from_str("video/mp4").map_err(internal_error)?,
    );

    // Ok(([(http::header::CONTENT_TYPE, "video/mp4")], file))

    Response::builder()
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"video.mp4\"",
        )
        .body(body)
        .map_err(internal_error)
}

#[axum::debug_handler]
async fn delete_link(
    State(app_state): State<AppState>,
    extract::Path(link_id): extract::Path<String>,
) -> Result<(), (StatusCode, String)> {
    use crate::schema::links;

    let conn = app_state.pool.get().await.map_err(internal_error)?;

    let id = link_id;

    debug!("deleting {}", id);

    let res = conn
        .interact(|conn| {
            diesel::delete(links::table)
                .filter(links::dsl::id.eq(id))
                .returning(Link::as_returning())
                .get_result(conn)
        })
        .await
        .map_err(internal_error)?
        .map_err(internal_error)?;
    let transcode_path = PathBuf::from(app_state.transcode_folder).join(res.id);

    let res = fs::remove_file(&transcode_path).await;

    match res {
        Ok(()) => {
            debug!("deleted {:?}", transcode_path.to_str())
        }
        Err(e) => {
            warn!("error when deleting file: {:?}", e);
        }
    }

    Ok(())
}

fn internal_error<E>(err: E) -> (StatusCode, String)
where
    E: std::error::Error,
{
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

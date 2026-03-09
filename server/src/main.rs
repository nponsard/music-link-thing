use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Output,
};

use axum::{
    Json, Router,
    extract::{self, State},
    http::{Method, StatusCode, header::CONTENT_TYPE},
    routing::{delete, get, post},
};
use deadpool_diesel::Pool;
use diesel::prelude::*;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use dotenvy::dotenv;
use sha2::{Digest, Sha256};
use tokio::{fs, process::Command, sync::mpsc::Sender};
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

async fn process_link(
    pool: &Pool<deadpool_diesel::Manager<SqliteConnection>>,
    link: &Link,
    download_folder: &String,
    transcode_folder: &String,
) -> Result<(), Box<dyn Error>> {
    use crate::schema::links;

    let download_path = PathBuf::from(download_folder).join(link.id.as_str());
    let transcode_path = PathBuf::from(transcode_folder).join(link.id.as_str());
    let conn = pool.get().await?;

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
        .await??;

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
        .await??;

        return Ok(());
    }

    let mut cmd = Command::new("yt-dlp");
    let download_path_str = download_path.to_str().ok_or("cannot convert path")?;
    cmd.arg("-o")
        .arg(&download_path)
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

    let _ = cmd.output().await?;

    if !download_path.exists() {
        return Err("downloaded file not found".into());
    }

    debug!("hashig file");
    let hash = hash_file(&download_path)?;
    let hash_insert = Some(hash.clone());

    let id = link.id.clone();

    let result = conn
        .interact(|conn| {
            links::table
                .filter(links::original_hash.eq(hash))
                .load::<Link>(conn)
        })
        .await??;
    conn.interact(|conn| {
        diesel::update(links::dsl::links)
            .filter(links::dsl::id.eq(id))
            .set(links::dsl::original_hash.eq(hash_insert))
            .execute(conn)
    })
    .await??;

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
            .await?;

        let parsed: FfprobeOutput = serde_json::from_slice(&stdout)?;

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

            let _ = Command::new("ffmpeg").arg(&image_path).output().await?;

            if image_path.exists() {
                command
                    .arg("-i")
                    .arg(&image_path)
                    .arg("-map")
                    .arg("0:a:0")
                    .arg("-map")
                    .arg("1:v:0")
                    .arg("-pix_fmt")
                    .arg("yuv420p")
                    .arg("-tune")
                    .arg("stillimage");
            } else {
                command
                    .arg("-filter_complex")
                    .arg("[0:a]avectorscope=s=1280x720");
                complex_filter = true;
            }
        }

        if !complex_filter {
            command.arg("-vf").arg("scale=-2:720:flags=lanczos");
        }

        command
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

        let result = command.output().await?;

        debug!("ffmpeg result: {:?}", result);
    }

    fs::remove_file(download_path).await?;

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

        if let Err(e) = result {
            error!("Failed to process link {},{}", link.url, e.to_string())
        }
    }
}

#[derive(Clone)]
struct AppState {
    pool: Pool<deadpool_diesel::Manager<SqliteConnection>>,
    control_tx: Sender<Link>,
    transcode_folder: String,
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
    tokio::spawn(async move {
        tasks_manager(
            task_pool,
            &mut control_rx,
            download_folder,
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
        .route("/link/{id}", delete(delete_link))
        .with_state(AppState {
            pool,
            control_tx,
            transcode_folder,
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
        original_hash: None,
        transcoded_hash: None,
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

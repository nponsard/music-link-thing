// @generated automatically by Diesel CLI.

diesel::table! {
    links (id) {
        id -> Text,
        url -> Text,
        original_hash -> Nullable<Text>,
        transcoded_hash -> Nullable<Text>,
    }
}

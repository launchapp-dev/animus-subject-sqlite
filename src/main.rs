use animus_plugin_protocol::{PluginInfo, PLUGIN_KIND_SUBJECT_BACKEND};
use animus_plugin_runtime::subject_backend_main;
use animus_subject_sqlite::backend::SqliteBackend;
use animus_subject_sqlite::config::SqliteConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    emit_manifest_if_requested();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let config = SqliteConfig::from_env()?;
    let backend = SqliteBackend::new(config).await?;

    let info = PluginInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        plugin_kind: PLUGIN_KIND_SUBJECT_BACKEND.into(),
        description: Some(env!("CARGO_PKG_DESCRIPTION").into()),
    };

    subject_backend_main(info, backend).await
}

fn emit_manifest_if_requested() {
    if !std::env::args()
        .skip(1)
        .any(|arg| arg == "--manifest" || arg == "-m")
    {
        return;
    }

    let manifest = serde_json::json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "plugin_kind": "subject_backend",
        "description": env!("CARGO_PKG_DESCRIPTION"),
        "protocol_version": animus_plugin_protocol::PROTOCOL_VERSION,
        "capabilities": [
            "task/list",
            "task/get",
            "task/update",
            "task/schema",
            "task/watch",
            "subject/list",
            "subject/get",
            "subject/update",
            "subject/schema",
            "subject/watch",
            "health/check",
            "subject_kind:task"
        ],
        "env_required": [
            {
                "name": "ANIMUS_SQLITE_DB_PATH",
                "description": "Path to the SQLite database file.",
                "required": false
            },
            {
                "name": "ANIMUS_SQLITE_KINDS",
                "description": "Comma-separated subject kinds served by this backend.",
                "required": false
            },
            {
                "name": "ANIMUS_SQLITE_AUTO_MIGRATE",
                "description": "Run schema migrations on startup.",
                "required": false
            }
        ]
    });
    println!(
        "{}",
        serde_json::to_string(&manifest).expect("serialize manifest")
    );
    std::process::exit(0);
}

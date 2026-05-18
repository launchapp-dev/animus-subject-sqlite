use animus_plugin_protocol::{PluginInfo, PLUGIN_KIND_SUBJECT_BACKEND};
use animus_plugin_runtime::subject_backend_main;
use animus_subject_sqlite::backend::SqliteBackend;
use animus_subject_sqlite::config::SqliteConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

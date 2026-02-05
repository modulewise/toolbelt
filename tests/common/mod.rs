use rmcp::ClientHandler;
use rmcp::model::ClientInfo;
use std::io::Write;
use std::ops::Deref;
use std::path::Path;
use tempfile::{Builder, NamedTempFile};

#[macro_export]
macro_rules! args {
    ($($json:tt)+) => {
        serde_json::json!($($json)+).as_object().unwrap().clone()
    };
}

pub struct TestFile(NamedTempFile);

impl Deref for TestFile {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

pub fn create_wasm_test_file(wat_content: &str) -> TestFile {
    let component_bytes = wat::parse_str(wat_content).unwrap();
    let mut temp_file = Builder::new().suffix(".wasm").tempfile().unwrap();
    temp_file.write_all(&component_bytes).unwrap();
    TestFile(temp_file)
}

/// A component that exports add-two(x: s32) -> s32
pub fn add_two_component() -> TestFile {
    let wat = r#"
        (component
            (core module $m
                (func $add_two (param i32) (result i32)
                    local.get 0
                    i32.const 2
                    i32.add
                )
                (export "add-two" (func $add_two))
            )
            (core instance $i (instantiate $m))
            (func $f (param "x" s32) (result s32) (canon lift (core func $i "add-two")))
            (export "add-two" (func $f))
        )
    "#;
    create_wasm_test_file(wat)
}

#[derive(Debug, Clone, Default)]
pub struct TestClientHandler;

impl ClientHandler for TestClientHandler {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

pub struct TestClient {
    pub client: Option<rmcp::service::RunningService<rmcp::RoleClient, TestClientHandler>>,
    server_handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl std::ops::Deref for TestClient {
    type Target = rmcp::service::RunningService<rmcp::RoleClient, TestClientHandler>;

    fn deref(&self) -> &Self::Target {
        self.client.as_ref().unwrap()
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        if let Some(client) = &self.client {
            client.cancellation_token().cancel();
        }
        if let Some(handle) = self.server_handle.take() {
            handle.abort();
        }
    }
}

impl TestClient {
    #[allow(dead_code)]
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(client) = self.client.take() {
            drop(client);
        }
        if let Some(handle) = self.server_handle.take() {
            handle.await??;
        }
        Ok(())
    }
}

pub async fn setup_test_client<H>(server_handler: H) -> TestClient
where
    H: rmcp::ServerHandler + 'static,
{
    use rmcp::ServiceExt;

    let (server_transport, client_transport) = tokio::io::duplex(4096);

    let server_handle = tokio::spawn(async move {
        server_handler
            .serve(server_transport)
            .await?
            .waiting()
            .await?;
        anyhow::Ok(())
    });

    let client_handler = TestClientHandler::default();
    let client = client_handler.serve(client_transport).await.unwrap();

    TestClient {
        client: Some(client),
        server_handle: Some(server_handle),
    }
}

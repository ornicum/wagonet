use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait Transport: Send {
    async fn read_command(&mut self) -> Result<Option<(u32, u32)>>;
    async fn receive_data(&mut self, size: u32) -> Result<Vec<u8>>;
    async fn send_response(&mut self, status: u8, data: Option<&[u8]>) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
}

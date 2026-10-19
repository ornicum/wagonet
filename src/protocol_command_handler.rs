use async_trait::async_trait;
use std::error::Error;

use crate::protocol_structs::ResponseStatus;

#[async_trait]
pub trait ProtocolCommandHandler: Send + Sync + 'static {
    async fn run(
        &mut self,
        command: u32,
        data: Vec<u8>,
    ) -> Result<(ResponseStatus, Vec<u8>), Box<dyn Error + Send + Sync>>;
}

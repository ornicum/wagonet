pub const DEFAULT_MAX_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// Read exactly `buf.len()` bytes, distinguishing clean disconnects
/// from mid-frame truncation.
///
/// - Full read: `Ok(())`.
/// - Peer closed before any byte of the frame: `Err(Error::ConnectionClosed)`.
/// - Peer closed after a partial frame: `Err(Error::ConnectionClosedMidFrame)`.
pub(crate) async fn read_exact_or_closed<R>(reader: &mut R, buf: &mut [u8]) -> crate::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..]).await?;
        if n == 0 {
            return Err(if filled == 0 {
                crate::Error::ConnectionClosed
            } else {
                crate::Error::ConnectionClosedMidFrame {
                    received: filled,
                    expected: buf.len(),
                }
            });
        }
        filled += n;
    }
    Ok(())
}

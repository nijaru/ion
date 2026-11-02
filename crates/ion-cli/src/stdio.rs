//! Cancellable pipe/socket I/O, without Tokio's blocking stdio worker threads.
use std::{
    io,
    os::fd::{AsFd, AsRawFd, OwnedFd, RawFd},
    pin::Pin,
    task::{Context, Poll, ready},
};

use rustix::fs::{FileType, OFlags, fcntl_getfl, fcntl_setfl, fstat};
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf, unix::AsyncFd};

pub(super) enum Stdio {
    Pollable(AsyncFd<NonblockingFd>),
    File(tokio::fs::File),
}

pub(super) struct NonblockingFd {
    fd: OwnedFd,
    original_flags: OFlags,
}

impl AsRawFd for NonblockingFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Drop for NonblockingFd {
    fn drop(&mut self) {
        // Duplicated descriptors share status flags with their caller. Restore
        // those flags before releasing our descriptor, including on faults.
        if let Err(error) = fcntl_setfl(&self.fd, self.original_flags) {
            eprintln!("stdio flag restoration failed: {error}");
        }
    }
}

impl Stdio {
    pub(super) fn open(
        input: (impl AsFd, Interest),
        output: (impl AsFd, Interest),
    ) -> io::Result<(Self, Self)> {
        // Standard streams may alias one open file description (e.g. a socketpair).
        // Capture both original flags before making either nonblocking.
        let (input, input_interest) = input;
        let (output, output_interest) = output;
        let input = rustix::io::fcntl_dupfd_cloexec(input, 0)?;
        let output = rustix::io::fcntl_dupfd_cloexec(output, 0)?;
        let input_flags = fcntl_getfl(&input)?;
        let output_flags = fcntl_getfl(&output)?;
        Ok((
            Self::new(input, input_flags, input_interest)?,
            Self::new(output, output_flags, output_interest)?,
        ))
    }

    fn new(fd: OwnedFd, original_flags: OFlags, interest: Interest) -> io::Result<Self> {
        let kind = FileType::from_raw_mode(fstat(&fd)?.st_mode);
        if kind == FileType::RegularFile
            || (kind == FileType::CharacterDevice && !rustix::termios::isatty(&fd))
        {
            // Regular files and devices such as /dev/null cannot be registered
            // with epoll. Flush started file publication before releasing it.
            return Ok(Self::File(tokio::fs::File::from_std(fd.into())));
        }
        fcntl_setfl(&fd, original_flags | OFlags::NONBLOCK)?;
        Ok(Self::Pollable(AsyncFd::with_interest(
            NonblockingFd { fd, original_flags },
            interest,
        )?))
    }
}

impl AsyncRead for Stdio {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        match &mut *self {
            Self::File(file) => Pin::new(file).poll_read(cx, buffer),
            Self::Pollable(fd) => loop {
                let mut ready = ready!(fd.poll_read_ready(cx))?;
                match ready.try_io(|fd| {
                    rustix::io::read(&fd.get_ref().fd, buffer.initialize_unfilled())
                        .map_err(io::Error::from)
                }) {
                    Ok(Ok(count)) => {
                        buffer.advance(count);
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_) => continue,
                }
            },
        }
    }
}

impl AsyncWrite for Stdio {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match &mut *self {
            Self::File(file) => Pin::new(file).poll_write(cx, bytes),
            Self::Pollable(fd) => loop {
                let mut ready = ready!(fd.poll_write_ready(cx))?;
                match ready.try_io(|fd| {
                    rustix::io::write(&fd.get_ref().fd, bytes).map_err(io::Error::from)
                }) {
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(result) => return Poll::Ready(result),
                    Err(_) => continue,
                }
            },
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::File(file) => Pin::new(file).poll_flush(cx),
            Self::Pollable(_) => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn aliased_stdio_restores_flags_and_cancelled_reads_preserve_input() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let original = fcntl_getfl(&socket).unwrap();
        let (mut input, mut output) =
            Stdio::open((&socket, Interest::READABLE), (&socket, Interest::WRITABLE)).unwrap();
        assert!(fcntl_getfl(&socket).unwrap().contains(OFlags::NONBLOCK));
        let mut byte = [0];
        assert!(
            tokio::time::timeout(Duration::from_millis(10), input.read_exact(&mut byte))
                .await
                .is_err()
        );
        peer.write_all(b"r").unwrap();
        input.read_exact(&mut byte).await.unwrap();
        assert_eq!(&byte, b"r");
        output.write_all(b"w").await.unwrap();
        peer.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"w");
        // Darwin adds the read-only FWASWRITTEN bit after successful I/O.
        // Preserve that observation while restoring the caller's NONBLOCK bit.
        let expected =
            (fcntl_getfl(&socket).unwrap() & !OFlags::NONBLOCK) | (original & OFlags::NONBLOCK);
        drop(output);
        drop(input);
        assert_eq!(fcntl_getfl(&socket).unwrap(), expected);
    }
}

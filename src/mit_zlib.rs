/*
 *
 * This file is part of onizd, copyright ©2020 Solra Bizna.
 *
 * onizd is free software: you can redistribute it and/or modify it under the
 * terms of the GNU General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your option) any later
 * version.
 *
 * onizd is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
 * details.
 *
 * You should have received a copy of the GNU General Public License along with
 * onizd. If not, see <https://www.gnu.org/licenses/>.
 *
 */

//! `flate2`'s tokio support is too old and/or not applicable, so I get to roll
//! my own. Lovely.

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use flate2::{Compress, Decompress, Status, FlushCompress, FlushDecompress};
use std::{
    convert::TryInto,
    pin::Pin,
    mem::MaybeUninit,
    task::{Context, Poll},
};
use crate::errorize;

/// An `AsyncWrite` implementation that wraps `OwnedWriteHalf` and compresses
/// all data before being sent.
pub struct MitZlibWriter {
    inner: OwnedWriteHalf,
    zlib: Compress,
    buf: Vec<u8>,
    cursor: usize,
    unflushed_data_sent: bool,
}

impl MitZlibWriter {
    /// Flush any data that's currently in the buffer. Will **only** return
    /// `Poll::Ready(Ok(()))` if the buffer is now **empty**. In this case,
    /// `self.buf` will contain nothing, and `self.cursor` will be zero.
    fn soft_flush(&mut self, cx: &mut Context) -> Poll<std::io::Result<()>> {
        while self.cursor < self.buf.len() {
            let wrote = Pin::new(&mut self.inner)
                .poll_write(cx, &self.buf[self.cursor..]);
            match wrote {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(wat)) => return Poll::Ready(Err(wat)),
                Poll::Ready(Ok(wrote)) => self.cursor += wrote,
            }
        }
        self.buf.clear();
        self.cursor = 0;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for MitZlibWriter {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8])
                  -> Poll<std::io::Result<usize>> {
        let me = Pin::into_inner(self);
        match me.soft_flush(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(x)) => return Poll::Ready(Err(x)),
            _ => (),
        }
        if buf.is_empty() { return Poll::Ready(Ok(0)) }
        me.unflushed_data_sent = true;
        match me.zlib.compress_vec(buf, &mut me.buf,
                                     FlushCompress::None) {
            Ok(Status::Ok) => (),
            // This should not happen
            _ => return Poll::Ready(Err(errorize("compression \
                                                  error")))
        }
        // assume everything got buffered o_O
        match me.soft_flush(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(x)) => return Poll::Ready(Err(x)),
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(buf.len())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context)
                  -> Poll<std::io::Result<()>> {
        let me = Pin::into_inner(self);
        if me.unflushed_data_sent {
            match me.soft_flush(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(x)) => return Poll::Ready(Err(x)),
                _ => (),
            }
            match me.zlib.compress_vec(&[], &mut me.buf,
                                       FlushCompress::Sync) {
                Ok(Status::Ok) | Ok(Status::BufError) => (),
                // This should not happen
                _ => return Poll::Ready(Err(errorize("compression \
                                                      error")))
            }
            me.unflushed_data_sent = false;
        }
        match me.soft_flush(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(x)) => return Poll::Ready(Err(x)),
            _ => (),
        }
        Pin::new(&mut me.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context)
                  -> Poll<std::io::Result<()>> {
        Pin::new(&mut Pin::into_inner(self).inner).poll_shutdown(cx)
    }
}

/// An `AsyncRead` implementation that wraps a `OwnedReadHalf` and decompresses
/// any data that is received.
pub struct MitZlibReader {
    inner: OwnedReadHalf,
    zlib: Decompress,
    buf: Vec<u8>,
    cursor: usize,
}

impl AsyncRead for MitZlibReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context, buf: &mut[u8])
                 -> Poll<std::io::Result<usize>> {
        if buf.is_empty() { return Poll::Ready(Ok(0)) }
        let me = Pin::into_inner(self);
        loop {
            if me.cursor < me.buf.len() {
                let total_in_before = me.zlib.total_in();
                let total_out_before = me.zlib.total_out();
                match me.zlib.decompress(&me.buf[me.cursor..],
                                         buf, FlushDecompress::None) {
                    Ok(Status::Ok) => (),
                    Ok(Status::StreamEnd) => (), // ?????
                    // This should not happen
                    _ => return Poll::Ready(Err(errorize("decompression \
                                                          error 2")))
                }
                let total_in_after = me.zlib.total_in();
                let total_out_after = me.zlib.total_out();
                let read: usize = (total_out_after - total_out_before)
                    .try_into().unwrap();
                let wrote: usize = (total_in_after - total_in_before)
                    .try_into().unwrap();
                me.cursor += wrote;
                return Poll::Ready(Ok(read))
            }
            me.cursor = 0;
            me.buf.clear();
            match Pin::new(&mut me.inner).poll_read_buf(cx, &mut me.buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(x)) => return Poll::Ready(Err(x)),
                Poll::Ready(Ok(0)) => return Poll::Ready(Ok(0)),
                Poll::Ready(Ok(_)) => continue,
            }
        }
    }
    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut[MaybeUninit<u8>])
                                           -> bool {
        Pin::new(&self.inner).prepare_uninitialized_buffer(buf)
    }
}

/// Wraps an `OwnedWriteHalf`, compressing data before it's sent.
pub fn make_writer(inner: OwnedWriteHalf) -> MitZlibWriter {
    let zlib = Compress::new(flate2::Compression::best(), true);
    MitZlibWriter { zlib, inner, buf: Vec::with_capacity(256), cursor: 0,
                    unflushed_data_sent: false }
}

/// Wraps an `OwnedReadHalf`, decompressing data after it's received.
pub fn make_reader(inner: OwnedReadHalf, slice: &[u8]) -> MitZlibReader {
    let zlib = Decompress::new(true);
    let mut buf = Vec::with_capacity(256.max(slice.len()));
    buf.extend_from_slice(slice);
    MitZlibReader { zlib, inner, buf, cursor: 0 }
}

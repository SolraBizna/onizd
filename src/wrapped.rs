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

use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}},
};
use tokio_util::codec;
use std::{
    mem::MaybeUninit,
    pin::Pin,
    task::{Context, Poll},
};
use bytes::{Buf,BufMut};

use crate::{CompressionType, MessageCoder, Client, MitZlibReader, MitZlibWriter};

pub enum WrappedSocket {
    Uncompressed(OwnedReadHalf, OwnedWriteHalf),
    Zlib(MitZlibReader, MitZlibWriter),
}

impl AsyncRead for WrappedSocket {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context, buf: &mut[u8])
                 -> Poll<std::io::Result<usize>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref mut r, ref _w) =>
                Pin::new(r).poll_read(cx, buf),
            WrappedSocket::Zlib(ref mut r, ref _w) =>
                Pin::new(r).poll_read(cx, buf),
        }
    }
    fn poll_read_buf<B: BufMut>(self: Pin<&mut Self>, cx: &mut Context,
                                buf: &mut B)
                 -> Poll<std::io::Result<usize>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref mut r, ref _w) =>
                Pin::new(r).poll_read_buf(cx, buf),
            WrappedSocket::Zlib(ref mut r, ref _w) =>
                Pin::new(r).poll_read_buf(cx, buf),
        }
    }
    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut[MaybeUninit<u8>])
                                           -> bool {
        match self {
            WrappedSocket::Uncompressed(ref r, ref _w) =>
                r.prepare_uninitialized_buffer(buf),
            WrappedSocket::Zlib(ref r, ref _w) =>
                r.prepare_uninitialized_buffer(buf),
        }
    }
}

impl AsyncWrite for WrappedSocket {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8])
                  -> Poll<std::io::Result<usize>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref _r, ref mut w) =>
                Pin::new(w).poll_write(cx, buf),
            WrappedSocket::Zlib(ref _r, ref mut w) =>
                Pin::new(w).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context)
                  -> Poll<std::io::Result<()>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref _r, ref mut w) =>
                Pin::new(w).poll_flush(cx),
            WrappedSocket::Zlib(ref _r, ref mut w) =>
                Pin::new(w).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context)
                  -> Poll<std::io::Result<()>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref _r, ref mut w) =>
                Pin::new(w).poll_shutdown(cx),
            WrappedSocket::Zlib(ref _r, ref mut w) =>
                Pin::new(w).poll_shutdown(cx),
        }
    }
    fn poll_write_buf<B: Buf>(self: Pin<&mut Self>, cx: &mut Context,
                              buf: &mut B)
                  -> Poll<std::io::Result<usize>> {
        match Pin::into_inner(self) {
            WrappedSocket::Uncompressed(ref _r, ref mut w) =>
                Pin::new(w).poll_write_buf(cx, buf),
            WrappedSocket::Zlib(ref _r, ref mut w) =>
                Pin::new(w).poll_write_buf(cx, buf),
        }
    }
}

pub async fn wrap_client(orig: codec::Framed<TcpStream, MessageCoder>,
                              typ: Option<CompressionType>)
                              -> std::io::Result<Client> {
    let codec::FramedParts { io, codec, mut read_buf, write_buf, ..}
      = orig.into_parts();
    let (reader, mut writer) = io.into_split();
    writer.write_all(&write_buf[..]).await?;
    let wrapped_sock = match typ {
        None => WrappedSocket::Uncompressed(reader, writer),
        Some(CompressionType::Zlib) => {
            let splat = read_buf.split_to(read_buf.len());
            WrappedSocket::Zlib(crate::mit_zlib::make_reader(reader,
                                                             &splat[..]),
                                crate::mit_zlib::make_writer(writer))
        }
    };
    let mut new_parts = codec::FramedParts::new(wrapped_sock, codec);
    new_parts.read_buf.put(&read_buf[..]);
    Ok(codec::Framed::from_parts(new_parts))
}

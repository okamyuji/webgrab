//! 応答本文のContent-Encodingの判定と、展開後サイズに上限を掛けた逐次展開。
//! reqwestの自動展開は同名ヘッダの1行目しか見ずに全行を消すため、全値をここで検査する。

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 本文取得とrobots.txt取得で送るAccept-Encoding。
pub const ACCEPT_ENCODING: &str = "gzip, deflate, br";

/// 展開方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    Identity,
    Gzip,
    Deflate,
    Br,
}

/// Content-Encodingの全行・全値から展開方式を1つに決める。空と`identity`は無視する。
/// 展開できない値や2つ以上の値が残れば、その値をカンマ区切りで返す。
pub fn coding_of<'a>(values: impl Iterator<Item = &'a [u8]>) -> Result<Coding, String> {
    let tokens: Vec<&[u8]> = values
        .flat_map(|v| v.split(|&b| b == b','))
        .map(<[u8]>::trim_ascii)
        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case(b"identity"))
        .collect();
    let known = match tokens.as_slice() {
        [] => Some(Coding::Identity),
        [t] if t.eq_ignore_ascii_case(b"gzip") => Some(Coding::Gzip),
        [t] if t.eq_ignore_ascii_case(b"deflate") => Some(Coding::Deflate),
        [t] if t.eq_ignore_ascii_case(b"br") => Some(Coding::Br),
        _ => None,
    };
    known.ok_or_else(|| {
        tokens
            .iter()
            .map(|t| String::from_utf8_lossy(t))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// 展開の失敗の種類。
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// 展開後のバイト数が上限を超えた。
    OverCap,
    /// 圧縮データが壊れているか、途中で切れている。
    Corrupt(String),
}

/// 上限を超える書き込みを拒む出力先。展開器がエラーを包み直しても区別できるよう、
/// 上限超過は共有フラグに記録する。
struct CappedSink {
    buf: Vec<u8>,
    cap: u64,
    over: Arc<AtomicBool>,
}

impl Write for CappedSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len() as u64 + data.len() as u64 > self.cap {
            self.over.store(true, Ordering::Relaxed);
            return Err(io::Error::other("decoded size exceeds the cap"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// zlib形式（HTTPの`deflate`）の展開器。flate2の書き込み型デコーダは、途中で切れた入力を
/// finishで検出しないため、`StreamEnd`に達したかを自分で記録する。
struct ZlibWriter {
    state: flate2::Decompress,
    out: CappedSink,
    ended: bool,
}

const ZLIB_CHUNK: usize = 1 << 15;

impl Write for ZlibWriter {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let len = input.len();
        let mut buf = vec![0u8; ZLIB_CHUNK];
        // 入力を使い切った後も、展開器に残る出力が出なくなるまで呼び続ける。
        while !self.ended {
            let before_in = self.state.total_in();
            let before_out = self.state.total_out();
            let status = self
                .state
                .decompress(input, &mut buf, flate2::FlushDecompress::None)
                .map_err(io::Error::other)?;
            let used = (self.state.total_in() - before_in) as usize;
            let produced = (self.state.total_out() - before_out) as usize;
            self.out.write_all(&buf[..produced])?;
            input = &input[used..];
            self.ended = status == flate2::Status::StreamEnd;
            if (used, produced) == (0, 0) {
                if input.is_empty() {
                    break;
                }
                return Err(io::Error::other("deflate stream made no progress"));
            }
        }
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Inner {
    Identity(CappedSink),
    Gzip(flate2::write::MultiGzDecoder<CappedSink>),
    Deflate(ZlibWriter),
    Br(Box<brotli_decompressor::DecompressorWriter<CappedSink>>),
}

/// 受信したチャンクを順に渡して展開する。展開後のバイト数は`cap`を超えない。
pub struct Decoder {
    inner: Inner,
    over: Arc<AtomicBool>,
    received: bool,
}

const BROTLI_BUFFER: usize = 4096;

impl Decoder {
    pub fn new(coding: Coding, cap: u64) -> Self {
        let over = Arc::new(AtomicBool::new(false));
        let sink = CappedSink {
            buf: Vec::new(),
            cap,
            over: Arc::clone(&over),
        };
        let inner = match coding {
            Coding::Identity => Inner::Identity(sink),
            Coding::Gzip => Inner::Gzip(flate2::write::MultiGzDecoder::new(sink)),
            Coding::Deflate => Inner::Deflate(ZlibWriter {
                state: flate2::Decompress::new(true),
                out: sink,
                ended: false,
            }),
            Coding::Br => Inner::Br(Box::new(brotli_decompressor::DecompressorWriter::new(
                sink,
                BROTLI_BUFFER,
            ))),
        };
        Decoder {
            inner,
            over,
            received: false,
        }
    }

    fn classify(&self, e: io::Error) -> DecodeError {
        if self.over.load(Ordering::Relaxed) {
            DecodeError::OverCap
        } else {
            DecodeError::Corrupt(e.to_string())
        }
    }

    /// 1チャンクを展開する。
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), DecodeError> {
        self.received |= !chunk.is_empty();
        let r = match &mut self.inner {
            Inner::Identity(s) => s.write_all(chunk),
            Inner::Gzip(d) => d.write_all(chunk),
            Inner::Deflate(d) => d.write_all(chunk),
            Inner::Br(d) => d.write_all(chunk),
        };
        r.map_err(|e| self.classify(e))
    }

    /// 受信を打ち切り、それまでに展開できたバイト数を返す。
    pub fn abort(self) -> u64 {
        match self.finish_counted() {
            Ok(body) => body.len() as u64,
            Err((_, n)) => n,
        }
    }

    /// 末尾まで展開し、展開後の本文を返す。途中で切れた圧縮データは`Corrupt`になる。
    /// 1バイトも届かなかった本文は、符号化の指定があっても空の本文として返す。
    pub fn finish(self) -> Result<Vec<u8>, DecodeError> {
        self.finish_counted().map_err(|(e, _)| e)
    }

    /// `finish`と同じだが、失敗したときはそれまでに展開できたバイト数も返す。
    /// gzipとbrotliの展開器は出力を内部に貯めるので、閉じて吐き出させてから数える。
    pub fn finish_counted(self) -> Result<Vec<u8>, (DecodeError, u64)> {
        if !self.received {
            return Ok(Vec::new());
        }
        let over = &self.over;
        close_inner(self.inner).map(|s| s.buf).map_err(|(e, n)| {
            let e = if over.load(Ordering::Relaxed) {
                DecodeError::OverCap
            } else {
                DecodeError::Corrupt(e.to_string())
            };
            (e, n)
        })
    }
}

/// 展開器を閉じて出力先を取り出す。失敗したときは、それまでに書き出せたバイト数を添える。
fn close_inner(inner: Inner) -> Result<CappedSink, (io::Error, u64)> {
    let len = |s: &CappedSink| s.buf.len() as u64;
    match inner {
        Inner::Identity(s) => Ok(s),
        Inner::Gzip(mut d) => match d.try_finish() {
            Ok(()) => {
                let n = len(d.get_ref());
                d.finish().map_err(|e| (e, n))
            }
            Err(e) => Err((e, len(d.get_ref()))),
        },
        Inner::Deflate(d) if d.ended => Ok(d.out),
        Inner::Deflate(d) => Err((io::Error::other("truncated deflate stream"), len(&d.out))),
        // closeはエラーを返す前に、展開器に残った出力を書き出す。
        Inner::Br(mut d) => match d.close() {
            Ok(()) => d.into_inner().map_err(|s| {
                let n = len(&s);
                (io::Error::other("brotli writer was not closed"), n)
            }),
            // shortcut: brotliは途中で切れると展開済みの出力を窓（最大16MiB）の中に残し、closeでも
            // 全部は書き出さない。正確に数えられないので上限まで使ったとみなす。
            Err(e) => Err((e, d.get_ref().cap)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coding(values: &[&[u8]]) -> Result<Coding, String> {
        coding_of(values.iter().copied())
    }

    fn compress(c: Coding, data: &[u8]) -> Vec<u8> {
        match c {
            Coding::Identity => data.to_vec(),
            Coding::Gzip => {
                let mut e =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            Coding::Deflate => {
                let mut e =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            Coding::Br => include_bytes!("../tests/fixtures/article.html.br").to_vec(),
        }
    }

    fn decode(c: Coding, data: &[u8], cap: u64) -> Result<Vec<u8>, DecodeError> {
        let mut d = Decoder::new(c, cap);
        for chunk in data.chunks(7) {
            d.push(chunk)?;
        }
        d.finish()
    }

    #[test]
    fn coding_ignores_empty_and_identity_values() {
        assert_eq!(coding(&[]), Ok(Coding::Identity));
        assert_eq!(coding(&[b"", b"  ", b" Identity "]), Ok(Coding::Identity));
        assert_eq!(coding(&[b"identity", b"GZIP"]), Ok(Coding::Gzip));
        assert_eq!(coding(&[b"Deflate"]), Ok(Coding::Deflate));
        assert_eq!(coding(&[b" br "]), Ok(Coding::Br));
    }

    #[test]
    fn coding_rejects_unknown_and_stacked_values() {
        for (values, shown) in [
            (&[&b"compress"[..]][..], "compress"),
            (&[b"COMPRESS"], "COMPRESS"),
            (&[b" gzip, compress "], "gzip, compress"),
            (&[b"x-unknown"], "x-unknown"),
            (&[b"zstd"], "zstd"),
            (&[b"identit\xff"], "identit\u{fffd}"),
            (&[b"br", b"zstd"], "br, zstd"),
            (&[b"gzip", b"identity", b"gzip"], "gzip, gzip"),
        ] {
            assert_eq!(coding(values), Err(shown.to_string()));
        }
    }

    #[test]
    fn decoder_round_trips_each_coding() {
        let text = b"<p>webgrab content encoding round trip</p>".repeat(20);
        for c in [Coding::Identity, Coding::Gzip, Coding::Deflate] {
            assert_eq!(
                decode(c, &compress(c, &text), 1 << 20).unwrap(),
                text,
                "{c:?}"
            );
        }
        let br = decode(Coding::Br, &compress(Coding::Br, b""), 1 << 20).unwrap();
        assert!(String::from_utf8_lossy(&br).contains("SENTINEL-BR-4d1e"));
    }

    #[test]
    fn decoder_handles_output_larger_than_one_buffer_from_one_chunk() {
        let text = vec![b'y'; 200_000];
        for c in [Coding::Gzip, Coding::Deflate] {
            let mut d = Decoder::new(c, 1 << 20);
            d.push(&compress(c, &text)).unwrap();
            assert_eq!(d.finish().unwrap(), text, "{c:?}");
        }
    }

    #[test]
    fn decoder_stops_at_the_cap_after_decoding() {
        let text = vec![b'x'; 10_000];
        for c in [Coding::Identity, Coding::Gzip, Coding::Deflate] {
            assert_eq!(
                decode(c, &compress(c, &text), 9_999),
                Err(DecodeError::OverCap)
            );
            assert_eq!(
                decode(c, &compress(c, &text), 10_000).unwrap().len(),
                10_000
            );
        }
        let br = compress(Coding::Br, b"");
        assert_eq!(decode(Coding::Br, &br, 10), Err(DecodeError::OverCap));
    }

    #[test]
    fn decoder_returns_empty_body_when_no_bytes_arrive() {
        // 本文の無い応答にもContent-Encodingを付けるサーバーがある。
        for c in [Coding::Gzip, Coding::Deflate, Coding::Br] {
            assert_eq!(decode(c, b"", 1 << 20), Ok(Vec::new()), "{c:?}");
        }
    }

    const CAP: u64 = 1 << 20;

    #[test]
    fn failed_streams_report_everything_decoded_so_far() {
        use std::io::Read;
        let text = b"<p>truncated</p>".repeat(500);
        for c in [Coding::Gzip, Coding::Deflate, Coding::Br] {
            let full = compress(c, &text);
            for cut in (1..full.len()).step_by(13).map(|n| &full[..n]) {
                // 参照の展開器が途中で失敗するまでに返した量が、展開できた全量。
                let mut decodable = Vec::new();
                let _ = match c {
                    Coding::Gzip => {
                        flate2::read::MultiGzDecoder::new(cut).read_to_end(&mut decodable)
                    }
                    Coding::Deflate => {
                        flate2::read::ZlibDecoder::new(cut).read_to_end(&mut decodable)
                    }
                    _ => Ok(0),
                };
                // brotliは展開済みの量を数えきれないので、上限まで使ったとみなす。
                let expected = if c == Coding::Br {
                    CAP
                } else {
                    decodable.len() as u64
                };
                let push_all = || {
                    let mut d = Decoder::new(c, CAP);
                    for chunk in cut.chunks(16384) {
                        d.push(chunk).unwrap();
                    }
                    d
                };
                let at = cut.len();
                match push_all().finish_counted() {
                    Err((DecodeError::Corrupt(_), n)) => assert_eq!(n, expected, "{c:?} cut={at}"),
                    other => panic!("{c:?} cut={at}: {:?}", other.map(|b| b.len())),
                }
                assert_eq!(push_all().abort(), expected, "{c:?} cut={at}");
            }
        }
    }

    #[test]
    fn decoder_reports_truncated_or_corrupt_streams() {
        let text = b"<p>truncated</p>".repeat(50);
        for c in [Coding::Gzip, Coding::Deflate, Coding::Br] {
            let full = compress(c, &text);
            let cut = &full[..full.len() / 2];
            assert!(
                matches!(decode(c, cut, 1 << 20), Err(DecodeError::Corrupt(_))),
                "{c:?}"
            );
            assert!(
                matches!(
                    decode(c, b"not compressed at all", 1 << 20),
                    Err(DecodeError::Corrupt(_))
                ),
                "{c:?}"
            );
        }
    }
}

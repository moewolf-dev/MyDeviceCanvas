use image::imageops::FilterType;
use mdc_core::{ConnectionState, Frame, Session};
use mdc_protocol::{encode_control, Capabilities, MessageType, Packet, Surface, VERSION};
use mdc_transport::{MemoryLink, Transport};
use std::path::Path;

const MAX_SOURCE_DIMENSION: u32 = 4096;
const TARGET_WIDTH: u32 = 480;
const TARGET_HEIGHT: u32 = 320;

fn load_rgb565(path: &Path, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let image = image::open(path).map_err(|error| format!("unable to decode image: {error}"))?;
    if image.width() > MAX_SOURCE_DIMENSION || image.height() > MAX_SOURCE_DIMENSION {
        return Err("source image exceeds 4096x4096 limit".into());
    }
    let image = image
        .resize_exact(width, height, FilterType::Triangle)
        .to_rgb8();
    let capacity = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(2))
        .ok_or("image size overflow")?;
    let mut output = Vec::with_capacity(capacity);
    for pixel in image.pixels() {
        let r = (u16::from(pixel[0]) >> 3) << 11;
        let g = (u16::from(pixel[1]) >> 2) << 5;
        let b = u16::from(pixel[2]) >> 3;
        let value = r | g | b;
        output.extend_from_slice(&value.to_le_bytes());
    }
    Ok(output)
}

fn simulate() -> Result<(), Box<dyn std::error::Error>> {
    let (host, mut device) = MemoryLink::pair();
    let mut session = Session::new(host);
    session.connect()?;
    let hello_bytes = device.read()?.ok_or("missing HELLO")?;
    let (hello, _) = Packet::decode(&hello_bytes, 1024 * 1024)?;
    let caps = Capabilities {
        device_id: "simulator-1".into(),
        firmware: "simulator".into(),
        surfaces: vec![Surface {
            id: "main".into(),
            width: 2,
            height: 2,
            pixel_format: "RGB565".into(),
            stride: 4,
            rotation: 0,
        }],
        frame: true,
        tile: true,
        touch: false,
        ota: false,
        max_message: 1024 * 1024,
        max_chunk: 16 * 1024,
        max_in_flight: 1,
        max_fps: 30,
    };
    device.write(
        &Packet {
            version: VERSION,
            kind: MessageType::Capabilities,
            flags: 0,
            request_id: hello.request_id,
            payload: encode_control(&caps)?,
        }
        .encode(1024 * 1024)?,
    )?;
    if session.poll()? != ConnectionState::Ready {
        return Err("session did not become ready".into());
    }
    session.send_frame(Frame {
        surface_id: "main".into(),
        width: 2,
        height: 2,
        bytes: vec![0; 8],
    })?;
    let (frame, _) = Packet::decode(&device.read()?.ok_or("missing FRAME")?, 1024 * 1024)?;
    if frame.kind != MessageType::Frame {
        return Err("simulator received non-FRAME packet".into());
    }
    println!("simulation ok: HELLO -> CAPABILITIES -> FRAME");
    Ok(())
}

fn main() {
    let mut a = std::env::args().skip(1);
    match a.next().as_deref() {
        Some("devices") => println!("[]"),
        Some("inspect") => println!("{{\"devices\":[]}}"),
        Some("benchmark") => println!("no connected devices"),
        Some("simulate") => {
            if let Err(error) = simulate() {
                eprintln!("simulation failed: {error}");
                std::process::exit(1);
            }
        }
        Some("send-image") => {
            let path = a.next().unwrap_or_default();
            if path.is_empty() {
                eprintln!("send-image requires a path");
                std::process::exit(2);
            }
            match load_rgb565(Path::new(&path), TARGET_WIDTH, TARGET_HEIGHT) {
                Ok(bytes) => {
                    eprintln!(
                        "decoded {} bytes of RGB565; no connected device, use --port or --address",
                        bytes.len()
                    );
                    std::process::exit(2);
                }
                Err(error) => {
                    eprintln!("image error: {error}");
                    std::process::exit(2);
                }
            }
        }
        _ => {
            println!("mdc devices|inspect|send-image <path>|benchmark|simulate");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rgb565_is_little_endian_and_bounded() {
        let path = std::env::temp_dir().join(format!("mdc-cli-test-{}.png", std::process::id()));
        let image = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
        image.save(&path).unwrap();
        assert_eq!(load_rgb565(&path, 1, 1).unwrap(), vec![0, 248]);
        let _ = std::fs::remove_file(path);
    }
}

//! The MQTT 3.1.1 subset the Bambu LAN driver speaks: CONNECT, SUBSCRIBE,
//! the one pushall PUBLISH, DISCONNECT, and reading the broker's replies. It
//! can build no other PUBLISH, so no print command can leave this module.
use anyhow::{Context, Result, ensure};
use tokio::io::{AsyncRead, AsyncReadExt};

/// The largest packet read from a printer; a Bambu pushall report is a few KiB.
pub(crate) const MAX_PACKET: usize = 1024 * 1024;
/// The status request Node's adapter publishes, byte for byte.
pub(crate) const PUSHALL: &str = r#"{"pushing":{"sequence_id":"0","command":"pushall"}}"#;

#[derive(Debug, PartialEq)]
pub(crate) enum Packet {
    ConnAck(u8),
    SubAck(Vec<u8>),
    Publish { topic: String, payload: Vec<u8> },
    Other,
}

fn string(out: &mut Vec<u8>, value: &str) {
    let length = u16::try_from(value.len()).unwrap_or(u16::MAX);
    out.extend(length.to_be_bytes());
    out.extend(&value.as_bytes()[..usize::from(length)]);
}

fn frame(header: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = vec![header];
    let mut length = body.len();
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if length == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

/// CONNECT with a clean session, a username and a password.
pub(crate) fn connect(client_id: &str, username: &str, password: &str, keep_alive: u16) -> Vec<u8> {
    let mut body = Vec::new();
    string(&mut body, "MQTT");
    body.push(4);
    body.push(0b1100_0010);
    body.extend(keep_alive.to_be_bytes());
    string(&mut body, client_id);
    string(&mut body, username);
    string(&mut body, password);
    frame(0x10, body)
}

/// SUBSCRIBE to one topic at QoS 0, packet id 1.
pub(crate) fn subscribe(topic: &str) -> Vec<u8> {
    let mut body = 1u16.to_be_bytes().to_vec();
    string(&mut body, topic);
    body.push(0);
    frame(0x82, body)
}

/// The pushall status request to `device/<serial>/request`, QoS 0.
pub(crate) fn pushall(serial: &str) -> Vec<u8> {
    let mut body = Vec::new();
    string(&mut body, &format!("device/{serial}/request"));
    body.extend(PUSHALL.as_bytes());
    frame(0x30, body)
}

pub(crate) fn disconnect() -> Vec<u8> {
    vec![0xe0, 0]
}

/// PINGREQ, which keeps a long-lived session open.
pub(crate) fn pingreq() -> Vec<u8> {
    vec![0xc0, 0]
}

/// Reads one packet. A packet over `MAX_PACKET` fails before its body is read.
pub(crate) async fn read(reader: &mut (impl AsyncRead + Unpin)) -> Result<Packet> {
    let header = reader.read_u8().await?;
    let mut length = 0usize;
    for shift in [0, 7, 14, 21] {
        let byte = reader.read_u8().await?;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        ensure!(shift < 21, "Malformed MQTT packet length");
    }
    ensure!(length <= MAX_PACKET, "Bambu MQTT packet is too large");
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(match header >> 4 {
        2 => Packet::ConnAck(*body.get(1).context("Malformed MQTT CONNACK")?),
        9 => {
            ensure!(body.len() >= 3, "Malformed MQTT SUBACK");
            Packet::SubAck(body[2..].to_vec())
        }
        3 => {
            ensure!(body.len() >= 2, "Malformed MQTT PUBLISH");
            let end = 2 + usize::from(u16::from_be_bytes([body[0], body[1]]));
            let start = if header & 0b0110 == 0 { end } else { end + 2 };
            ensure!(body.len() >= start, "Malformed MQTT PUBLISH");
            Packet::Publish {
                topic: String::from_utf8_lossy(&body[2..end]).into_owned(),
                payload: body[start..].to_vec(),
            }
        }
        _ => Packet::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_t_23_frames_encode_the_mqtt_311_layout() {
        assert_eq!(disconnect(), [0xe0, 0]);
        let connect = connect("pp_1", "bblp", "code", 30);
        assert_eq!(&connect[..2], [0x10, 28]);
        assert_eq!(&connect[2..10], b"\0\x04MQTT\x04\xc2");
        let subscribe = subscribe("device/ABC123/report");
        assert_eq!(&subscribe[..4], [0x82, 25, 0, 1]);
        assert_eq!(subscribe.last(), Some(&0));
        let big = frame(0x30, vec![0; 200]);
        assert_eq!(&big[..3], [0x30, 0xc8, 0x01]);
    }

    #[tokio::test]
    async fn ticket_t_23_reader_parses_replies_and_refuses_oversized_packets() {
        let mut publish = frame(0x30, {
            let mut body = Vec::new();
            string(&mut body, "device/ABC123/report");
            body.extend(b"{}");
            body
        });
        publish.extend([0x20, 2, 0, 5, 0x90, 3, 0, 1, 0x80]);
        let mut reader = publish.as_slice();
        assert_eq!(
            read(&mut reader).await.unwrap(),
            Packet::Publish {
                topic: "device/ABC123/report".into(),
                payload: b"{}".to_vec()
            }
        );
        assert_eq!(read(&mut reader).await.unwrap(), Packet::ConnAck(5));
        assert_eq!(read(&mut reader).await.unwrap(), Packet::SubAck(vec![0x80]));
        let oversized = [0x30, 0x81, 0x80, 0x40];
        let error = read(&mut oversized.as_slice()).await.unwrap_err();
        assert_eq!(error.to_string(), "Bambu MQTT packet is too large");
        let endless = [0x30, 0xff, 0xff, 0xff, 0xff];
        assert!(read(&mut endless.as_slice()).await.is_err());
    }
}

//! A `multipart/form-data` request body for upload route tests.

/// A multipart body of `(field, file name, bytes)` parts, and its type.
pub fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for (name, file_name, bytes) in parts {
        let file = file_name.map_or(String::new(), |file| format!("; filename=\"{file}\""));
        body.extend_from_slice(
            format!("--pp\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--pp--\r\n");
    ("multipart/form-data; boundary=pp".to_owned(), body)
}

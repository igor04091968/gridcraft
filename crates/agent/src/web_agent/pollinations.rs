//! Read bounded provider errors without returning echoed prompts or credentials to the UI.
use std::time::Duration;

use serde_json::Value;

use super::settings;

pub(super) fn completion(url: &str, key: &str, body: &Value) -> Result<Value, String> {
    let client =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(90))).max_redirects(0).http_status_as_error(false).build().new_agent();
    let mut response = client.post(url).header("Authorization", &format!("Bearer {key}")).send_json(body).map_err(settings::http_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let error = response.body_mut().with_config().limit(64 * 1024).read_json::<Value>().unwrap_or(Value::Null);
        return Err(response_error(status, &error));
    }
    response.body_mut().with_config().limit(1024 * 1024).read_json().map_err(|_| "Некорректный ответ Pollinations.".into())
}

fn response_error(status: u16, error: &Value) -> String {
    if status != 400 {
        return settings::http_error(ureq::Error::StatusCode(status));
    }
    // Only classify known reasons. Raw messages can echo the API key or workbook contents.
    let details = ["/error/message", "/error/code", "/error/type", "/message", "/code", "/error"]
        .into_iter()
        .filter_map(|path| error.pointer(path).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if ["invalid model", "unknown model", "model not found", "unsupported model", "model_not_found", "invalid_model"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Pollinations не распознал выбранную модель (HTTP 400). Обновите список моделей и выберите модель из каталога; также можно указать псевдоним openai.".into()
    } else if ["context_length_exceeded", "maximum context length", "context window", "too many tokens", "input is too long", "prompt is too long"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Контекст превышает лимит модели Pollinations (HTTP 400). Укажите меньший диапазон ячеек, сократите задачу или выберите модель с большим контекстом.".into()
    } else if ["unsupported parameter", "unsupported_parameter", "unknown parameter", "not supported", "unrecognized parameter"]
        .iter()
        .any(|reason| details.contains(reason))
    {
        "Выбранная модель Pollinations не поддерживает параметры запроса (HTTP 400). Попробуйте другую модель из каталога.".into()
    } else {
        "Pollinations отклонил запрос (HTTP 400). Проверьте выбранную модель и диапазон ячеек; точная причина не распознана. Книга не изменена."
            .into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn classifies_bad_requests_without_echoing_provider_text() {
        for (message, expected) in [
            ("Invalid model or alias: sk-private-test", "не распознал"),
            ("context_length_exceeded: private workbook cell text", "Контекст превышает"),
            ("Unsupported parameter max_completion_tokens", "не поддерживает параметры"),
            ("Authorization Bearer sk-private-test; password=secret", "точная причина не распознана"),
        ] {
            let error = response_error(400, &json!({"error":{"message":message}}));
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("sk-private-test"));
            assert!(!error.contains("private workbook"));
            assert!(!error.contains("password="));
            assert!(!error.contains("Bearer"));
        }
        assert!(response_error(401, &json!({"message":"secret"})).contains("API key"));
        assert!(response_error(429, &Value::Null).contains("limit"));
    }

    #[test]
    fn http_400_body_is_read_and_a_subsequent_completion_still_works() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (status, body) in [
                ("400 Bad Request", r#"{"error":{"message":"Invalid model or alias: sk-private-test"}}"#),
                ("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    assert!(request.len() < 16 * 1024);
                    if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let body = json!({"model":"unknown", "messages":[{"role":"user","content":"test"}]});
        let error = completion(&url, "sk-private-test", &body).unwrap_err();
        assert!(error.contains("не распознал"));
        assert!(!error.contains("sk-private-test"));
        assert_eq!(completion(&url, "sk-private-test", &body).unwrap()["choices"][0]["message"]["content"], "ok");
        server.join().unwrap();
    }
}

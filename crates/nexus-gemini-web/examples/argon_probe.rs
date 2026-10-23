//! 用一份 Cookie 打一次 4 Argon。只打印服务端自报的模型名和回复摘要。

use nexus_gemini_web::session;

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).expect("cookie file");
    let cookie = std::fs::read_to_string(&path).expect("read cookie");
    let client = session::http_client();
    let rotate = client
        .post("https://accounts.google.com/RotateCookies")
        .header("Content-Type", "application/json")
        .header("Origin", "https://accounts.google.com")
        .header("Cookie", cookie.trim())
        .body("[000,\"-0000000000000000000\"]")
        .send()
        .await
        .expect("rotate");
    let names: Vec<_> = rotate
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split('=').next())
        .map(|s| s.to_string())
        .collect();
    println!("rotate_status={} set_cookie_names={names:?}", rotate.status());
    let cookie = session::refresh_session(&client, cookie.trim()).await;
    println!("rotated_has_psidts={}", cookie.contains("__Secure-1PSIDTS="));
    match session::generate(
        &client,
        &cookie,
        "gemini-4-argon",
        "Reply with one short sentence: what is 17+4?",
        None,
        &mut |_thinking, _delta| {},
    )
    .await
    {
        Ok(output) => {
            println!(
                "upstream_model={}",
                output.upstream_model.unwrap_or_default()
            );
            let preview: String = output.text.chars().take(180).collect();
            println!(
                "text_chars={} preview={}",
                output.text.chars().count(),
                preview.replace('\n', " ")
            );
            println!("thinking_chars={}", output.thinking.chars().count());
            println!("follow={}", output.follow.is_some());
        }
        Err(err) => println!("error={err}"),
    }
}

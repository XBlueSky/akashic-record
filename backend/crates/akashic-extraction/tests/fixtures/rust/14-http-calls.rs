// C1 client HTTP-call extraction fixture: literal-path reqwest calls only.
async fn fetch_widget() {
    // method-style call, bare path literal → http_call "GET /api/widget".
    let _ = client.get("/api/widget").await;
    // free-function call, full URL → reduced to "POST /api/things".
    let _ = reqwest::post("https://svc.example/api/things").await;
    // interpolated path (not a string literal) → NOT captured.
    let _ = client.get(format!("/api/thing/{}", id)).await;
    // map-style get with a non-URL literal → NOT captured (URL-shape guard).
    let _ = cache.get("some_key");
}

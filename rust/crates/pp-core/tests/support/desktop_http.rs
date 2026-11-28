use pp_core::CoreRuntime;
use reqwest::{Client, StatusCode, header};
use std::time::Duration;

pub async fn connect(runtime: &mut CoreRuntime, wait: Duration) -> (Client, String) {
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(wait)
        .build()
        .unwrap();
    let bootstrap = client
        .get(runtime.take_launch_target().unwrap().into_url())
        .send()
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
    let cookie =
        pp_testkit::session_cookie(bootstrap.headers()[header::SET_COOKIE].to_str().unwrap());
    (client, cookie)
}

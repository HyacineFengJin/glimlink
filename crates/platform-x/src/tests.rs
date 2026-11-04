// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;

fn user() -> Value {
    json!({"__typename":"User","rest_id":"42","core":{"name":"Fixture","screen_name":"fixture"},"profile_bio":{"description":"bio"},"relationship_counts":{"followers":"7","following":3},"tweet_counts":{"tweets":8},"privacy":{"protected":false},"avatar":{"image_url":"https://pbs.twimg.com/profile_images/fixture.jpg"}})
}
fn tweet(id: &str) -> Value {
    json!({"__typename":"Tweet","rest_id":id,"core":{"user_results":{"result":user()}},"details":{"full_text":"body","created_at_ms":123456789,"conversation_id":"111"},"counts":{"favorite_count":7,"reply_count":2,"retweet_count":3,"quote_count":1,"bookmark_count":4},"views":{"count":"19"},"media_entities2":[{"type":"animated_gif","media_url_https":"https://pbs.twimg.com/tweet_video_thumb/fixture.jpg","original_info":{"width":80,"height":60},"ext_alt_text":"alt","video_info":{"variants":[{"url":"https://video.twimg.com/tweet_video/fixture.mp4","content_type":"video/mp4","bitrate":832000},{"url":"https://evil.invalid/a.mp4","content_type":"video/mp4"}]}}]})
}
fn entry(t: Value) -> Value {
    json!({"entry_id":"tweet-fixture","content":{"__typename":"TimelineTimelineItem","content":{"__typename":"TimelineTweet","tweet_results":{"result":t}}}})
}
fn timeline(instructions: Value) -> Value {
    json!({"data":{"user_result_by_screen_name":{"result":{"__typename":"User","profile_user_originals_timeline":{"timeline":{"instructions":instructions}}}}}})
}

#[test]
fn session_import_keeps_only_required_cookies_and_debug_is_redacted() {
    let session = Session::from_private_input(
        "Cookie: ignored=opaque; auth_token=secret-auth; ct0=secret-csrf; twid=u%3D123",
    )
    .unwrap();
    assert_eq!(
        session.cookie_header(),
        "auth_token=secret-auth; ct0=secret-csrf"
    );
    assert!(!format!("{session:?}").contains("secret"));
    let encoded = serde_json::to_string(&session).unwrap();
    let decoded: Session = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.cookie_header(), session.cookie_header());
    assert!(Session::from_private_input("auth_token=a; auth_token=b; ct0=c").is_err());
    assert!(Session::from_private_input("auth_token=a\r\nX-Injection: yes; ct0=c").is_err());
    assert!(Session::from_private_input(r#"{"auth_token":"a;ct0=b","ct0":"c"}"#).is_err());
}
#[test]
fn legacy_and_modern_username_variables_have_distinct_wire_names() {
    let legacy = super::user_name_variables(false, "fixture");
    assert_eq!(legacy["screen_name"], "fixture");
    assert!(legacy.get("screenName").is_none());
    let modern = super::user_name_variables(true, "fixture");
    assert_eq!(modern["screenName"], "fixture");
    assert!(modern.get("screen_name").is_none());
}
#[test]
fn modern_user_identity_and_counts_are_preserved() {
    let value = json!({"data":{"user_result_by_screen_name":{"result":user()}}});
    let u = model::user_response(&value).unwrap();
    assert_eq!(u.id, "42");
    assert_eq!(u.followers_count, Some(7));
    assert_eq!(u.posts_count, Some(8));
    assert!(model::user_response(&json!({"data":{"user_result_by_screen_name":{"result":{"__typename":"UserUnavailable"}}}})).is_err());
}
#[test]
fn rich_post_uses_exact_id_longform_and_lazy_gif_variants() {
    let mut t = tweet("111");
    t["note_tweet"] = json!({"note_tweet_results":{"result":{"text":"full longform"}}});
    let value = json!({"data":{"tweet_result_by_rest_id":{"result":t}}});
    let t = model::tweet_response(&value, "111").unwrap();
    assert_eq!(t.text, "full longform");
    assert_eq!(t.counts.like, Some(7));
    assert_eq!(t.counts.view, Some(19));
    assert_eq!(t.media.len(), 1);
    assert_eq!(t.media[0].variants.len(), 1);
    assert!(matches!(t.media[0].kind, MediaKind::AnimatedGif));
    assert_eq!(t.media[0].width, Some(80));
    assert!(model::tweet_response(&value, "999").is_err());
}
#[test]
fn modern_timeline_pin_module_cursor_and_reused_cursor_remain_explicit() {
    let value = timeline(json!([
        {"__typename":"TimelinePinEntry","entry":entry(tweet("100"))},
        {"__typename":"TimelineAddEntries","entries":[entry(tweet("100")),entry(tweet("101")),{"entry_id":"cursor-bottom","content":{"__typename":"TimelineTimelineCursor","cursor_type":"Bottom","value":"same","stop_on_empty_response":true}}]},
        {"__typename":"TimelineAddToModule","module_items":[{"item":{"content":{"tweet_results":{"result":tweet("102")}}}}]}
    ]));
    let p = model::timeline_response(&value, "42", Some("same"), 20).unwrap();
    assert_eq!(p.timeline_kind, "original_posts");
    assert_eq!(p.pinned.len(), 1);
    assert_eq!(p.items.len(), 2);
    assert_eq!(p.next_cursor.as_deref(), Some("same"));
    assert!(p.pagination_note.unwrap().contains("deduplicate"));
    assert_eq!(p.has_more, Some(true));
}
#[test]
fn account_home_queries_have_distinct_shapes_and_do_not_accept_profile_or_wrong_feed() {
    let instructions = json!([{"__typename":"TimelineAddEntries","entries":[entry(tweet("111")),{"content":{"cursor_type":"Bottom","value":"next-home"}}]}]);
    let recommended =
        json!({"data":{"home_timeline":{"home_timeline_urt":{"instructions":instructions}}}});
    let parsed = model::home_response(&recommended, true, None, 100).unwrap();
    assert_eq!(parsed.timeline_kind, "for_you");
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.next_cursor.as_deref(), Some("next-home"));
    assert!(model::home_response(&recommended, false, None, 100).is_err());
    let following = json!({"data":{"home_latest_timeline":{"home_timeline_urt":{"instructions":instructions}}}});
    assert_eq!(
        model::home_response(&following, false, None, 100)
            .unwrap()
            .timeline_kind,
        "following"
    );
    assert!(model::home_response(&following, true, None, 100).is_err());
    assert!(model::home_response(&timeline(instructions.clone()), true, None, 100).is_err());
    assert!(model::home_response(&json!({"data":{}}), true, None, 100).is_err());
    let legacy = json!({"data":{"home":{"home_timeline_urt":{"instructions":instructions}}}});
    assert_eq!(
        model::home_response(&legacy, true, None, 100)
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn home_variables_follow_the_published_relay_contract_and_cursor_stays_explicit() {
    let first = super::home_variables(true, true, None, 20);
    assert_eq!(first["requestContext"], "launch");
    assert!(first.get("cursor").is_none());
    assert!(first.get("enableRanking").is_none());
    let next = super::home_variables(true, false, Some("provider-cursor"), 10);
    assert_eq!(next["requestContext"], "scroll");
    assert_eq!(next["cursor"], "provider-cursor");
    assert_eq!(next["enableRanking"], false);
    assert!(next.get("userId").is_none());
    assert!(next.get("screenName").is_none());
}
#[test]
fn modern_author_results_and_wrapper_without_outer_id_are_not_lost() {
    let mut post = tweet("111");
    let author = post["core"]["user_results"]["result"].take();
    post.as_object_mut().unwrap().remove("core");
    post["author_results"] = json!({"result":author});
    let raw = json!({"data":{"tweet_result_by_rest_id":{"result":{"__typename":"TweetWithVisibilityResults","tweet":post}}}});
    let parsed = model::tweet_response(&raw, "111").unwrap();
    assert_eq!(parsed.id, "111");
    assert_eq!(parsed.author.id, "42");
    assert_eq!(parsed.text, "body");
    let page = timeline(
        json!([{"__typename":"TimelineAddEntries","entries":[entry(raw["data"]["tweet_result_by_rest_id"]["result"].clone())]}]),
    );
    assert_eq!(
        model::timeline_response(&page, "42", None, 20)
            .unwrap()
            .items
            .len(),
        1
    );
}
#[test]
fn truncation_never_suggests_lossless_paging_and_empty_never_loops() {
    let value = timeline(
        json!([{"__typename":"TimelineAddEntries","entries":[entry(tweet("101")),entry(tweet("102")),{"content":{"cursor_type":"Bottom","value":"cursor"}}]}]),
    );
    let p = model::timeline_response(&value, "42", None, 1).unwrap();
    assert!(p.truncated);
    assert!(p.next_cursor.is_none());
    assert_eq!(p.server_next_cursor.as_deref(), Some("cursor"));
    let empty = timeline(
        json!([{"__typename":"TimelineAddEntries","entries":[{"content":{"cursor_type":"Bottom","value":"cursor"}}]}]),
    );
    let p = model::timeline_response(&empty, "42", Some("cursor"), 20).unwrap();
    assert!(p.next_cursor.is_none());
    assert_eq!(p.has_more, Some(true));
    assert!(model::timeline_response(&json!({"data":{}}), "42", None, 20).is_err());
}
#[test]
fn errors_never_echo_server_message_or_credential_material() {
    for (code, error) in [
        (89, Error::LoginRequired),
        (326, Error::VerificationRequired),
        (179, Error::PermissionDenied),
        (144, Error::NotFound),
    ] {
        let actual =
            model::check_errors(&json!({"errors":[{"code":code,"message":"secret-auth body"}]}))
                .unwrap_err();
        assert_eq!(actual, error);
        assert!(!actual.to_string().contains("secret"));
    }
    assert_eq!(
        model::check_errors(&json!({"errors":[{"code":991}]})).unwrap_err(),
        Error::ProtocolChanged
    );
}
#[test]
fn media_hosts_and_public_script_hosts_are_exact() {
    assert!(model::media_url(
        "https://pbs.twimg.com/media/fixture.jpg?format=jpg&name=orig"
    ));
    assert!(model::media_url("https://video.twimg.com/a.mp4"));
    for url in [
        "http://pbs.twimg.com/a",
        "https://pbs.twimg.com.evil/a",
        "https://pbs.twimg.com@evil/a",
        "https://user@pbs.twimg.com/a",
        "https://pbs.twimg.com/a#f",
        "https://api.x.com/graphql/id/name",
    ] {
        assert!(!model::media_url(url), "{url}");
    }
    assert!(transport::script_url(
        "https://abs.twimg.com/x-web/x-web/assets/sign.o-hash.js"
    ));
    assert!(!transport::script_url(
        "https://abs.twimg.com/x-web/x-web/../evil.js"
    ));
    assert!(!transport::script_url(
        "https://evil.invalid/x-web/x-web/a.js"
    ));
}

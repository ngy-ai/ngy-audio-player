use std::path::Path;
use std::time::UNIX_EPOCH;

use super::plan::CAPABILITIES;
use super::user_choice::{base64, hash_input, timestamp_hex, user_choice_hash};
use super::*;

fn plan_for(exe: &str) -> Vec<Entry> {
    plan(Path::new(exe))
}

fn find<'a>(entries: &'a [Entry], key_suffix: &str, name: Option<&str>) -> Option<&'a Entry> {
    entries
        .iter()
        .find(|entry| entry.key.ends_with(key_suffix) && entry.name.as_deref() == name)
}

#[test]
fn command_quotes_the_exe_and_takes_one_argument() {
    let entries = plan_for("C:\\Program Files\\声波\\ngy-audio-player.exe");
    let command = find(&entries, "shell\\open\\command", None).expect("缺少打开命令");
    assert_eq!(
        command.data,
        "\"C:\\Program Files\\声波\\ngy-audio-player.exe\" \"%1\""
    );
    let icon = find(&entries, "DefaultIcon", None).expect("缺少图标");
    assert!(
        icon.data.ends_with("\",0"),
        "图标同样要带引号：{}",
        icon.data
    );
}

#[test]
fn every_registered_extension_gets_both_entries() {
    let entries = plan_for("C:\\app.exe");
    let exts = extensions();
    for ext in &exts {
        assert!(
            find(
                &entries,
                &format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
                Some(PROG_ID)
            )
            .is_some(),
            "`{ext}` 没进打开方式"
        );
        assert!(
            find(
                &entries,
                "\\Capabilities\\FileAssociations",
                Some(&format!(".{ext}"))
            )
            .is_some(),
            "`{ext}` 没进默认应用页的能力表"
        );
    }
    // 能力表里不该混进视频容器。
    assert!(
        find(&entries, "\\Capabilities\\FileAssociations", Some(".mkv")).is_none(),
        "视频容器不该出现在登记清单里"
    );
}

#[test]
fn app_is_advertised_to_the_settings_page() {
    let entries = plan_for("C:\\app.exe");
    let registered = find(&entries, "RegisteredApplications", Some(APP_KEY))
        .expect("没在 RegisteredApplications 里登记，系统设置里就不会出现");
    assert_eq!(registered.data, CAPABILITIES);
    assert_eq!(
        find(&entries, "Capabilities", Some("ApplicationName"))
            .expect("缺少应用名")
            .data,
        "声波"
    );
}

#[test]
fn unregister_removes_both_trees() {
    let roots = delete_roots();
    assert!(roots.iter().any(|root| root.ends_with(PROG_ID)));
    assert!(
        roots
            .iter()
            .any(|root| root.ends_with("Software\\Soundwave"))
    );
}

/// 三个向量由参考实现（PS-SFTA 的原版 PowerShell 代码）在固定输入下算出。
/// 移植只要有一处移位方向、常量或字节序写错，这里就会对不上。
#[test]
fn hash_matches_the_reference_implementation() {
    let sid = "s-1-5-21-1111111111-2222222222-3333333333-1001";
    let stamp = "01d9d0a1b2c3d4e5";
    assert_eq!(
        user_choice_hash(&hash_input("mp3", sid, stamp)),
        "FlKzJsb+8YI="
    );
    assert_eq!(
        user_choice_hash(&hash_input("flac", sid, stamp)),
        "SvHWGghlvOM="
    );
    assert_eq!(
        user_choice_hash(&hash_input("cue", sid, stamp)),
        "sRvW+NvUcl0="
    );
}

/// 手动验收用：会**真的**改本机的默认程序，所以默认忽略。
/// `cargo test -p ngy-audio-player --lib force_defaults_touches_real_system -- --ignored --nocapture`
#[test]
#[ignore = "会改写本机文件关联"]
fn force_defaults_touches_real_system() {
    let report = force_defaults().expect("写 UserChoice 失败");
    println!("applied={} failures={:?}", report.applied, report.failures);
    assert!(report.applied > 0);
}

#[test]
fn timestamp_is_truncated_to_the_minute() {
    // 2026-10-27T00:00:00Z 正好是整分。
    let start = UNIX_EPOCH + std::time::Duration::from_secs(1_792_059_180);
    assert_eq!(
        timestamp_hex(start),
        timestamp_hex(start + std::time::Duration::from_secs(59)),
        "同一分钟内的哈希必须可以互换，否则算哈希和写键会错开"
    );
    assert_ne!(
        timestamp_hex(start),
        timestamp_hex(start + std::time::Duration::from_secs(60))
    );
    assert_eq!(timestamp_hex(start).len(), 16);
}

#[test]
fn base64_pads_like_the_real_thing() {
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(&[0xff]), "/w==");
}

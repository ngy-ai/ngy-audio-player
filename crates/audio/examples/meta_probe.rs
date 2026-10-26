fn main() {
    let path = std::path::Path::new("testdata/demo.mp3");
    let meta = ngy_audio_engine::TrackMeta::probe(path);
    println!(
        "title={:?} artist={:?} album={:?}",
        meta.title, meta.artist, meta.album
    );
    println!(
        "duration={:?} rate={:?} bitrate={:?}",
        meta.duration, meta.sample_rate, meta.bitrate_kbps
    );
    println!("cover={:?}", meta.cover);
}

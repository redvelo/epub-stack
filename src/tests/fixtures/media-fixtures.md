# Media Fixture Provenance

These files contain only synthetic sine waves and test patterns generated from FFmpeg
`lavfi` sources. They contain no third-party source recordings or images and are
contributed under the repository's license. They were generated with FFmpeg 8.1.2 using
the following commands from the repository root.

```sh
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.25" -ac 1 -b:a 64k -c:a libmp3lame -y src/tests/fixtures/media-cbr.mp3
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=660:sample_rate=44100:duration=0.4" -ac 1 -c:a libmp3lame -q:a 4 -y src/tests/fixtures/media-vbr.mp3
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.25" -ac 1 -c:a aac -f adts -y src/tests/fixtures/media.aac
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.25" -ac 1 -c:a libopus -y src/tests/fixtures/media-opus.ogg
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.25" -ac 1 -c:a libvorbis -y src/tests/fixtures/media-vorbis.ogg
ffmpeg -hide_banner -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.3" -ac 1 -c:a libopus -strict experimental -movflags +faststart -y src/tests/fixtures/media-opus.mp4
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=32x24:rate=10:duration=0.3" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.3" -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac -movflags +faststart -shortest -y src/tests/fixtures/media-h264-aac.mp4
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=32x24:rate=10:duration=0.3" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.3" -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac -shortest -y src/tests/fixtures/media-h264-aac-tail.mp4
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=32x24:rate=10:duration=0.3" -pix_fmt yuv420p -c:v libvpx -deadline realtime -cpu-used 8 -an -y src/tests/fixtures/media-vp8.webm
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=32x24:rate=10:duration=0.3" -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=0.3" -pix_fmt yuv420p -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -c:a libopus -shortest -y src/tests/fixtures/media-vp9-opus.webm
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc=size=32x24:rate=10:duration=0.3" -pix_fmt yuv420p -c:v libaom-av1 -cpu-used 8 -crf 40 -an -y src/tests/fixtures/media-av1.webm
```

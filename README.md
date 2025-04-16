### 실행 방법
실행 이전에 `ffmpeg`이 설치되어 있어야 합니다. <br>
참고: https://github.com/zmwangx/rust-ffmpeg/wiki/Notes-on-building

```bash
# 실행 위치
$ pwd
/your_current_directory/video_pubsub

# Publisher 실행
$ cargo run -p publisher -- --path /absolute_path_to_video/input.mp4

# Subscriber 실행
$ cargo run -p subscriber -- --path /absolute_path_to_video/output.mp4
```
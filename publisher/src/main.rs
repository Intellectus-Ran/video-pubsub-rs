use std::io::Cursor;
use dust_dds::domain::domain_participant_factory::DomainId;
use dust_dds::{
    domain::domain_participant_factory::DomainParticipantFactory,
    infrastructure::{
        qos::QosKind,
        status::{StatusKind, NO_STATUS},
        time::Duration,
        wait_set::{Condition, WaitSet},
    },
};
use ffmpeg_next::{format::input, frame::Video as VideoFrame, software::scaling::{context::Context, flag::Flags}};
use video_pubsub::models::{FrameData, VideoMetadata, DOMAIN_ID};
use clap::{arg, Parser};
use std::f64;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(short, long)]
    path: String,
}

fn main() {
    let args = Args::parse();

    // ffmpeg 초기화
    ffmpeg_next::init().unwrap();

    // DDS 초기화
    let participant_factory = DomainParticipantFactory::get_instance();

    let participant = participant_factory
        .create_participant(DOMAIN_ID as DomainId, QosKind::Default, None, NO_STATUS)
        .unwrap();

    // 퍼블리셔 생성
    let publisher = participant
        .create_publisher(QosKind::Default, None, NO_STATUS)
        .unwrap();

    // 비디오 파일 열기
    let mut input_context = input(&args.path).expect("Failed to open input file");

    println!("Input loaded from the path");

    // 비디오 스트림 중 가장 좋은(첫 번째) 스트림 선택
    let input = input_context
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| "no video stream found")
        .unwrap();

    let video_stream_index = input.index();

    // 스트림 파라미터로부터 디코더 생성
    let context_decoder = ffmpeg_next::codec::context::Context::from_parameters(input.parameters()).unwrap();
    let mut decoder = context_decoder.decoder().video().unwrap();

    println!("Decoder created");

    // RGB24로 변환할 스케일러 생성
    let mut scaler = Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg_next::format::Pixel::RGB24,
        decoder.width(),
        decoder.height(),
        Flags::BILINEAR,
    ).unwrap();

    println!("Scaler created");

    let mut frame_index = 0;

    // 프레임레이트 계산
    let mut fps: f64 = input.avg_frame_rate().into();
    fps = (fps * 100.0).round() / 100.0;  // 소수점 둘째자리까지 반올림
    let width = decoder.width();
    let height = decoder.height();

    // 비디오 메타데이터 구조체 생성
    let video_metadata = VideoMetadata {
        width,
        height,
        fps,
    };

    println!("Video metadata created: {:?}", video_metadata);

    // VideoMetadata 토픽 생성
    let metadata_topic = participant
        .create_topic::<VideoMetadata>(
            "VideoMetadata",
            "VideoMetadata",
            QosKind::Default,
            None,
            NO_STATUS,
        )
        .unwrap();

    // VideoMetadata용 데이터라이터 생성
    let matadata_writer = publisher
        .create_datawriter(&metadata_topic, QosKind::Default, None, NO_STATUS)
        .unwrap();
    let writer_cond = matadata_writer.get_statuscondition();
    writer_cond
        .set_enabled_statuses(&[StatusKind::PublicationMatched])
        .unwrap();
    let mut wait_set = WaitSet::new();
    wait_set
        .attach_condition(Condition::StatusCondition(writer_cond))
        .unwrap();

    // 구독자와 매칭될 때까지 대기
    wait_set.wait(Duration::new(60, 0)).unwrap();

    // Dust DDS에서 잠시 대기하지 않으면 구독자가 매칭되지 않는 경우가 종종 발생
    // Waitset이 완벽한 Match 완료 상태를 보장해주지 않는것으로 보임
    std::thread::sleep(std::time::Duration::from_millis(1000));
    println!("Matched with metadata subscriber");

    // 비디오 메타데이터 전송
    matadata_writer.write(&video_metadata, None).unwrap();
    println!("Sent video metadata");

    // FrameData 토픽 생성
    let framedata_topic = participant
        .create_topic::<FrameData>(
            "FrameData",
            "FrameData",
            QosKind::Default,
            None,
            NO_STATUS,
        )
        .unwrap();

    // FrameData용 데이터라이터 생성
    let framedata_writer = publisher
        .create_datawriter(&framedata_topic, QosKind::Default, None, NO_STATUS)
        .unwrap();
    let writer_cond = framedata_writer.get_statuscondition();
    writer_cond
        .set_enabled_statuses(&[StatusKind::PublicationMatched])
        .unwrap();
    let mut wait_set = WaitSet::new();
    wait_set
        .attach_condition(Condition::StatusCondition(writer_cond))
        .unwrap();

    // 구독자와 매칭될 때까지 대기
    wait_set.wait(Duration::new(60, 0)).unwrap();
    println!("Matched with frame data subscriber");

    // 비디오 프레임 패킷 반복 처리
    for (stream, packet) in input_context.packets() {
        // 비디오 스트림에 해당하는 패킷만 처리
        if stream.index() == video_stream_index {
            // 패킷을 디코더에 전달
            decoder.send_packet(&packet).unwrap();
            let mut decoded = VideoFrame::empty();

            // 디코드 된 프레임을 받을 수 있을 때
            while decoder.receive_frame(&mut decoded).is_ok() {
                let mut rgb_frame = VideoFrame::empty();
                // 프레임을 RGB24로 변환
                scaler.run(&decoded, &mut rgb_frame).unwrap();

                let data = rgb_frame.data(0);

                // raw RGB 데이터를 RgbImage로 변환
                let img = image::RgbImage::from_raw(width, height, data.to_vec())
                    .expect("Failed to create RgbImage");

                // RgbImage를 JPEG 바이트로 인코딩
                let mut jpeg_bytes: Vec<u8> = Vec::new();
                img.write_to(&mut Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
                    .expect("Failed at writing JPEG");

                // FrameData 메시지 생성
                let sample = FrameData {
                    index: frame_index,
                    byte_data: jpeg_bytes,
                };

                // 프레임 데이터 전송
                framedata_writer.write(&sample, None).unwrap();
                println!("Wrote sample: {:?}", sample.index);
                frame_index += 1;
            }
        }
    }

    // 종료 신호 전송 (빈 벡터)
    let sample = FrameData {
        index: frame_index,
        byte_data: Vec::new(),
    };

    framedata_writer.write(&sample, None).unwrap();
    println!("Wrote sample: {:?}", sample.index);

    // 디코더에 EOF 신호 전송
    decoder.send_eof().unwrap();

    // 데이터라이터 삭제
    publisher.delete_datawriter(&framedata_writer).unwrap();
}

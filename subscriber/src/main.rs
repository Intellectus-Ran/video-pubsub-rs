use dust_dds::domain::domain_participant_factory::DomainId;
use dust_dds::infrastructure::time::Duration;
use dust_dds::infrastructure::wait_set::{Condition, WaitSet};
use dust_dds::subscription::data_reader::DataReader;
use dust_dds::{
    domain::domain_participant_factory::DomainParticipantFactory,
    infrastructure::{
        qos::QosKind,
        status::{StatusKind, NO_STATUS},
    },
};
use ffmpeg_next;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::log::Level;
use ffmpeg_next::media::Type;
use ffmpeg_next::software::scaling::{context::Context as SwsContext, flag::Flags};
use ffmpeg_next::util::frame::Video as Frame;
use ffmpeg_next::{codec, Packet, Rational};
use image::{ImageReader};
use std::io::Cursor;
use video_pubsub::models::*;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(short, long)]
    path: String,
}

fn main() {
    let args = Args::parse();

    // ffmpeg 로그 레벨 설정 및 초기화
    ffmpeg_next::log::set_level(Level::Trace);
    ffmpeg_next::init().expect("Could not init ffmpeg");

    // H264 인코더 코덱 찾기 및 출력 파일 생성
    let codec = codec::encoder::find(codec::Id::H264).unwrap();
    let mut output = ffmpeg_next::format::output(&args.path).unwrap();

    // 컨테이너가 글로벌 헤더를 요구하는지 확인
    let global_header = output
        .format()
        .flags()
        .contains(ffmpeg_next::format::flag::Flags::GLOBAL_HEADER);

    // 비디오 인코더 컨텍스트 생성
    let mut encoder = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .unwrap();

    // 글로벌 헤더 플래그 설정
    if global_header {
        encoder.set_flags(ffmpeg_next::codec::flag::Flags::GLOBAL_HEADER);
    }

    // DDS 초기화
    let participant_factory = DomainParticipantFactory::get_instance();

    let participant = participant_factory
        .create_participant(DOMAIN_ID as DomainId, QosKind::Default, None, NO_STATUS)
        .unwrap();

    // 구독자 생성
    let subscriber = participant
        .create_subscriber(QosKind::Default, None, NO_STATUS)
        .unwrap();

    // VideoMetadata 토픽 생성 및 DataReader 준비
    let metadata_topic = participant
        .create_topic::<VideoMetadata>(
            "VideoMetadata",
            "VideoMetadata",
            QosKind::Default,
            None,
            NO_STATUS,
        )
        .unwrap();

    let metadata_reader: DataReader<VideoMetadata> = subscriber
        .create_datareader(&metadata_topic, QosKind::Default, None, NO_STATUS)
        .unwrap();

    // 구독 매칭 대기 조건 설정
    let metadata_reader_cond = metadata_reader.get_statuscondition();
    metadata_reader_cond
        .set_enabled_statuses(&[StatusKind::SubscriptionMatched])
        .unwrap();

    let mut metadata_waitset = WaitSet::new();
    metadata_waitset
        .attach_condition(Condition::StatusCondition(metadata_reader_cond.clone()))
        .unwrap();

    // 퍼블리셔와 매칭될 때까지 대기
    metadata_waitset.wait(Duration::new(20, 0)).unwrap();
    println!("Matched with metadata publisher");

    // 데이터 수신 가능 상태로 변경
    metadata_reader_cond
        .set_enabled_statuses(&[StatusKind::DataAvailable])
        .unwrap();

    // 실제 데이터가 도착할 때까지 대기
    metadata_waitset.wait(Duration::new(20, 0)).unwrap();
    println!("Metadata is available");

    // VideoMetadata 샘플 수신
    let metadata_sample = metadata_reader.read_next_sample().unwrap();

    // 메타데이터에서 해상도, fps 추출
    let width = metadata_sample.data().unwrap().width;
    let height = metadata_sample.data().unwrap().height;
    let fps = metadata_sample.data().unwrap().fps;
    let frame_rate: Rational = fps.into();
    let time_base: Rational = frame_rate.invert();

    println!(
        "Received metadata with width {} height {} and fps {}",
        width, height, fps
    );

    // 인코더에 메타데이터 기반 설정 적용
    encoder.set_format(Pixel::YUV420P);
    encoder.set_width(width);
    encoder.set_height(height);
    encoder.set_time_base(time_base);
    encoder.set_frame_rate(Some(frame_rate));

    // 비디오 스트림 생성 및 인덱스 저장
    let mut stream = output.add_stream(codec).expect("Could not add stream");
    let stream_index = stream.index();

    // 인코더 오픈
    let mut opened_encoder = encoder.open().expect("Could not open encoder");

    // 스트림에 인코더 파라미터 복사
    stream.set_parameters(&opened_encoder);

    // 스트림 time_base 명시적 설정
    stream.set_time_base(time_base);
    stream.set_rate(frame_rate);

    // 컨테이너 헤더 기록, 헤더 기록 전에 모든 설정이 끝나있어야 함
    output.write_header().unwrap();

    println!("Successfully initialized video encoder");

    // FrameData 토픽 생성 및 DataReader 준비
    let framedata_topic = participant
        .create_topic::<FrameData>("FrameData", "FrameData", QosKind::Default, None, NO_STATUS)
        .unwrap();

    let framedata_reader: DataReader<FrameData> = subscriber
        .create_datareader(&framedata_topic, QosKind::Default, None, NO_STATUS)
        .unwrap();

    // 구독 매칭 대기 조건 설정
    let framedata_reader_cond = framedata_reader.get_statuscondition();
    framedata_reader_cond
        .set_enabled_statuses(&[StatusKind::SubscriptionMatched])
        .unwrap();

    let mut waitset = WaitSet::new();
    waitset
        .attach_condition(Condition::StatusCondition(framedata_reader_cond.clone()))
        .unwrap();

    // 퍼블리셔와 매칭될 때까지 대기
    waitset.wait(Duration::new(20, 0)).unwrap();

    println!("Matched with frame data publisher");

    // RGB24 → YUV420P 변환용 scaler 생성
    let mut scaler = SwsContext::get(
        Pixel::RGB24,
        width,
        height,
        Pixel::YUV420P,
        width,
        height,
        Flags::BILINEAR,
    )
    .unwrap();

    let mut pts: i64 = 0; // 프레임 타임스탬프(pts) 초기화
    let mut pts_step: i64 = 1;
    let final_time_base = output.streams().best(Type::Video).unwrap().time_base();

    // output.write_header() 이후 time base가 자동으로 조정되고 이후 설정한대로 비디오가 인코딩이 안되어 pts step을 수정함으로써 해결
    if time_base != final_time_base {
        pts_step = ((time_base.numerator() * final_time_base.denominator())
            / (time_base.denominator() * final_time_base.numerator())) as i64;
        println!(
            "Pts step modified to {} to match the final time base {}",
            pts_step, final_time_base
        );
    }

    loop {
        // 데이터 수신 가능 상태로 변경
        framedata_reader_cond
            .set_enabled_statuses(&[StatusKind::DataAvailable])
            .unwrap();

        // 프레임 데이터 도착 대기
        waitset.wait(Duration::new(30, 0)).unwrap();

        // 프레임 데이터 샘플 수신
        let framedata_sample = framedata_reader.read_next_sample().unwrap();
        let frame_data = framedata_sample.data().unwrap();
        println!(
            "Received frame data {} with size: {} bytes",
            frame_data.index,
            frame_data.byte_data.len()
        );

        // byte_data가 비어 있으면 비디오의 끝이므로 종료
        if frame_data.byte_data.is_empty() {
            println!("Received end signal from publisher");
            break;
        }

        // JPEG 바이트 데이터를 이미지로 디코딩
        let img_reader = match ImageReader::new(Cursor::new(&frame_data.byte_data)).with_guessed_format() {
            Ok(reader) => reader,
            Err(e) => {
                println!("Failed to guess image format: {}", e);
                continue;
            }
        };

        let img = match img_reader.decode() {
            Ok(decoded) => decoded.to_rgb8(),
            Err(e) => {
                println!("Failed to decode from byte to rgb: {}", e);
                continue;
            }
        };

        // RGB24 프레임 생성 및 데이터 복사
        let mut input_frame = Frame::new(Pixel::RGB24, width, height);
        input_frame.data_mut(0).copy_from_slice(&img);

        // YUV420P 프레임 생성 및 pts 설정
        let mut output_frame = Frame::empty();
        output_frame.set_format(Pixel::YUV420P);
        output_frame.set_width(width);
        output_frame.set_height(height);
        output_frame.set_pts(Some(pts));

        // RGB → YUV 변환
        if let Err(e) = scaler.run(&input_frame, &mut output_frame) {
            println!("Could not scale frame from rgb to yuv: {}", e);
            continue;
        }

        // 변환된 비디오 프레임(YUV)을 FFmpeg 인코더에 전달
        if let Err(e) = opened_encoder.send_frame(&output_frame) {
            println!("Failed to send frame to encoder: {}", e);
            continue;
        }

        let mut packet = Packet::empty();

        // 인코더에서 압축된 비디오 스트림(패킷)을 받아 출력 파일에 기록
        while let Ok(()) = opened_encoder.receive_packet(&mut packet) {
            packet.set_stream(stream_index);
            if let Err(e) = packet.write_interleaved(&mut output) {
                println!("Failed to write packet to output file: {}", e);
                continue;
            }
        }

        pts += pts_step;
    }

    // 인코더에 EOF 신호 전송
    opened_encoder.send_eof().unwrap();
    println!("Sent EOF to encoder");

    // 남은 패킷들 받아서 기록
    let mut packet = Packet::empty();
    while let Ok(()) = opened_encoder.receive_packet(&mut packet) {
        packet.set_stream(stream_index);
        packet.write(&mut output).unwrap();
    }
    println!("Finished writing remaining packets");

    // 컨테이너 트레일러 기록
    output.write_trailer().unwrap();
    println!("Wrote trailer");
}

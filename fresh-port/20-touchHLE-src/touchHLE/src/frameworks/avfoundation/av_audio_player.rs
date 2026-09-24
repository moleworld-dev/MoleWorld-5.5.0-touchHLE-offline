/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! AVAudioPlayer
//!
//! Implemented using Audio Queue Services based on [the PlayingAudio example](https://developer.apple.com/library/archive/documentation/MusicAudio/Conceptual/AudioQueueProgrammingGuide/AQPlayback/PlayingAudio.html)

use crate::dyld::HostFunction;
use crate::frameworks::audio_toolbox::audio_file::{
    self, guest_audio_file_read_from_vec, kAudioFilePropertyDataFormat,
    kAudioFilePropertyPacketSizeUpperBound, kAudioFileReadPermission, AudioFileClose,
    AudioFileGetProperty, AudioFileID, AudioFileOpenURL, AudioFileReadPackets,
};
use crate::frameworks::audio_toolbox::audio_queue::{
    kAudioQueueParam_Volume, AudioQueueAllocateBuffer, AudioQueueBufferRef, AudioQueueDispose,
    AudioQueueEnqueueBuffer, AudioQueueGetParameter, AudioQueueNewOutput, AudioQueueOutputCallback,
    AudioQueuePause, AudioQueueRef, AudioQueueSetParameter, AudioQueueStart, AudioQueueStop,
};
use crate::frameworks::carbon_core::eofErr;
use crate::frameworks::core_audio_types::AudioStreamBasicDescription;
use crate::frameworks::core_foundation::cf_run_loop::kCFRunLoopCommonModes;
use crate::frameworks::foundation::ns_error::NSOSStatusErrorDomain;
use crate::frameworks::foundation::{ns_string, NSInteger, NSTimeInterval, NSUInteger};
use crate::mem::{guest_size_of, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, todo_objc_setter, Class,
    ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

const kNumberBuffers: usize = 3;

struct AVAudioPlayerHostObject {
    audio_file_url: id,
    output_callback: AudioQueueOutputCallback,
    audio_file_id: Option<AudioFileID>,
    audio_desc: Option<AudioStreamBasicDescription>,
    audio_queue: Option<AudioQueueRef>,
    audio_queue_buffers: Option<MutPtr<AudioQueueBufferRef>>,
    num_packets_to_read: u32,
    current_packet: i64,
    // The time set by calling setCurrentTime is stored here in case it's set
    // before prepareToPlay is called; so it can be applied when it's called
    set_current_time: NSTimeInterval,
    volume: f32,
    is_playing: bool,
    num_of_loops: NSInteger,
    // [扫描修 2026-09-16] 播放中读到文件末尾且不再循环(已发异步 AudioQueueStop)时置真。
    // 异步停止完成后 AudioQueueReset 会清空队列里的缓冲,之后若只 AudioQueueStart 就既没有
    // 缓冲可播、也不会再触发输出回调补数据,所以 play 看到它要先重新灌缓冲。
    finished: bool,
    // [扫描修 2026-09-16] 弱引用:iOS SDK 里 AVAudioPlayer.delegate 是 assign 属性,不 retain。
    delegate: id,
}
impl HostObject for AVAudioPlayerHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation AVAudioPlayer: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let symb = "__touchHLE_AVAudioPlayerOutputBufferHelper";
    let hf: HostFunction = &(_touchHLE_AVAudioPlayerOutputBufferHelper as fn(&mut Environment, _, _, _) -> _);
    let callback = env
        .dyld
        .create_guest_function(&mut env.mem, symb, hf);

    let host_object = Box::new(AVAudioPlayerHostObject {
        audio_file_url: nil,
        output_callback: callback,
        audio_file_id: None,
        audio_desc: None,
        audio_queue: None,
        audio_queue_buffers: None,
        num_packets_to_read: 0,
        current_packet: 0,
        set_current_time: 0.0,
        volume: 1.0,
        is_playing: false,
        num_of_loops: 0,
        finished: false,
        delegate: nil
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithContentsOfURL:(id)url // NSURL*
                      error:(MutPtr<id>)out_error { // NSError**
    let path: id = msg![env; url path];
    let path_str = ns_string::to_rust_string(env, path);
    log_dbg!("[(AVAudioPlayer*){:?} initWithContentsOfURL:{:?} {} outError:{:?}]", this, url, path_str, out_error);

    retain(env, url);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_file_url = url;

    // Check for errors. Return nil and write them to error if there are
    let tmp_afi_ptr: MutPtr<AudioFileID> = env.mem.alloc(guest_size_of::<AudioFileID>()).cast();
    let status = AudioFileOpenURL(env, url, kAudioFileReadPermission, 0, tmp_afi_ptr) as NSInteger;
    let audio_file_id = env.mem.read(tmp_afi_ptr);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_file_id = Some(audio_file_id);
    env.mem.free(tmp_afi_ptr.cast());
    if status != 0 {
        if !out_error.is_null() {
            let domain = ns_string::get_static_str(env, NSOSStatusErrorDomain);
            let error = msg_class![env; NSError alloc];
            let error = msg![env; error initWithDomain:domain code:status userInfo:nil];
            autorelease(env, error);
            env.mem.write(out_error, error);
        }
        release(env, this);
        return nil;
    }

    this
}

- (id)initWithData:(id)data // NSData*
             error:(MutPtr<id>)out_error { // NSError**
    let bytes: ConstVoidPtr = msg![env; data bytes];
    let length: NSUInteger = msg![env; data length];
    let data_vec = env
        .mem
        .bytes_at(bytes.cast(), length)
        .to_vec();

    assert_eq!(env.objc.borrow::<AVAudioPlayerHostObject>(this).audio_file_url, nil);

    match guest_audio_file_read_from_vec(env, data_vec) {
        Ok(audio_file_id) => {
            assert!(env.objc.borrow::<AVAudioPlayerHostObject>(this).audio_file_id.is_none());
            env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_file_id = Some(audio_file_id);
            this
        }
        Err(_) => {
            assert!(out_error.is_null()); // TODO
            release(env, this);
            nil
        }
    }
}

- (())setDelegate:(id)delegate {
    // [扫描修 2026-09-16] 只存值不 retain(assign 属性)。audioPlayerDidFinishPlaying:successfully:
    // 暂不发:要在队列真正停止时发,得挂 kAudioQueueProperty_IsRunning 监听(audio_queue.rs 里
    // AudioQueueAddPropertyListener 不是 pub),而且回调里释放播放器会让 handle_audio_queue 拿不到
    // 队列而 panic,需要延到 run loop 上发。本游戏 CDLongAudioSource 只把它转给 CDAudioManager,
    // 后者从没设过完成选择子(全二进制无 setBackgroundMusicCompletionListener:selector: 的 selref),不发没有影响。
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).delegate = delegate;
}
- (id)delegate {
    env.objc.borrow::<AVAudioPlayerHostObject>(this).delegate
}
- (())setMeteringEnabled:(bool)enabled {
    todo_objc_setter!(this, enabled);
}

- (f32)volume {
    let aq_ref = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_queue;
    if aq_ref.is_none() {
        // TODO: is it correct? can we always return it instead of querying?
        return env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).volume;
    }

    let tmp: MutPtr<f32> = env.mem.alloc(guest_size_of::<f32>()).cast();
    let status = AudioQueueGetParameter(env, aq_ref.unwrap(), kAudioQueueParam_Volume, tmp);
    assert_eq!(status, 0);
    let res = env.mem.read(tmp);
    env.mem.free(tmp.cast());
    res
}
- (())setVolume:(f32)volume {
    let host_object = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this);
    host_object.volume = volume;
    if let Some(aq_ref) = host_object.audio_queue {
        let status = AudioQueueSetParameter(env, aq_ref, kAudioQueueParam_Volume, volume);
        assert_eq!(status, 0);
    }
}

- (())prepareToPlay {
    let audio_queue = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_queue;
    if audio_queue.is_some() {
        return;
    }

    let audio_file_id = env.objc.borrow::<AVAudioPlayerHostObject>(this).audio_file_id.unwrap();
    let callback = env.objc.borrow::<AVAudioPlayerHostObject>(this).output_callback;

    let size = guest_size_of::<AudioStreamBasicDescription>();
    let tmp_size_ptr: MutPtr<GuestUSize> = env.mem.alloc(guest_size_of::<GuestUSize>()).cast();
    env.mem.write(tmp_size_ptr, size);
    let tmp_data_ptr: MutPtr<AudioStreamBasicDescription> = env.mem.alloc(size).cast();
    let status = AudioFileGetProperty(
        env, audio_file_id, kAudioFilePropertyDataFormat, tmp_size_ptr, tmp_data_ptr.cast()
    );
    assert_eq!(status, 0);
    assert_eq!(size, env.mem.read(tmp_size_ptr));
    let audio_desc = env.mem.read(tmp_data_ptr);
    log_dbg!("audio_desc {:?}", audio_desc);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_desc = Some(audio_desc);

    let aq_ref_ptr: MutPtr<AudioQueueRef> = env.mem.alloc(guest_size_of::<AudioQueueRef>()).cast();
    let common_modes = ns_string::get_static_str(env, kCFRunLoopCommonModes);
    let status = AudioQueueNewOutput(
        env, tmp_data_ptr.cast_const(), callback, this.cast(),
        Ptr::null(), common_modes, 0, aq_ref_ptr
    );
    assert_eq!(status, 0);
    let aq_ref = env.mem.read(aq_ref_ptr);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_queue = Some(aq_ref);

    // Reapply the previously set current time and volume in case
    // setVolume/setCurrentTime were called before prepareToPlay
    let volume = env.objc.borrow::<AVAudioPlayerHostObject>(this).volume;
    () = msg![env; this setVolume:volume];
    let set_current_time = env.objc.borrow::<AVAudioPlayerHostObject>(this).set_current_time;
    () = msg![env; this setCurrentTime:set_current_time];

    let size = guest_size_of::<u32>();
    env.mem.write(tmp_size_ptr, size);
    let prop_size_ptr: MutPtr<u32> = env.mem.alloc(size).cast();
    let status = AudioFileGetProperty(
        env, audio_file_id, kAudioFilePropertyPacketSizeUpperBound, tmp_size_ptr, prop_size_ptr.cast()
    );
    assert_eq!(status, 0);
    assert_eq!(size, env.mem.read(tmp_size_ptr));
    let prop_size = env.mem.read(prop_size_ptr);

    let (buffer_byte_size, num_packets_to_read) = derive_buffer_size(audio_desc, prop_size, 0.5);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).num_packets_to_read = num_packets_to_read;

    let buffers: MutPtr<AudioQueueBufferRef> = env.mem.alloc(kNumberBuffers as GuestUSize * guest_size_of::<AudioQueueBufferRef>()).cast();
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_queue_buffers = Some(buffers);

    for i in 0..kNumberBuffers {
        let status = AudioQueueAllocateBuffer(env, aq_ref, buffer_byte_size, buffers + i as u32);
        assert_eq!(status, 0);
    }
    prime_player_buffers(env, this, aq_ref, buffers);

    env.mem.free(tmp_size_ptr.cast());
    env.mem.free(aq_ref_ptr.cast());
    env.mem.free(tmp_data_ptr.cast());
}

- (bool)isPlaying {
    // [扫描修 2026-09-16] 已播完就不算在播。灌缓冲时一个包都没读到(起点在末尾或空文件)时 play 仍会把
    // is_playing 置真,靠 finished 纠正;CDAudioManager applicationWillResignActive 据此决定回前台是否续播。
    let host_object = env.objc.borrow::<AVAudioPlayerHostObject>(this);
    host_object.is_playing && !host_object.finished
}

- (bool)play {
    () = msg![env; this prepareToPlay];

    let aq_ref = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).audio_queue.unwrap();

    // [扫描修 2026-09-16] 同一实例自然播完后再 play 必须重新灌缓冲,否则无声:CDLongAudioSource load:
    // 遇到同一路径只 pause + rewind 就复用本播放器,村庄/黄金岛/夜晚环境音每轮连播 4 次,原先后 3 次都没声音。
    // 先立即停止:EOF 比 OpenAL 真正放完早约 1 秒,那时缓冲可能还在队列里,直接重新入队会重复入队;
    // 立即停止会同步 AudioQueueReset,保证缓冲全部出队。不用 Dispose 重建队列:AudioQueueDispose
    // 不删 OpenAL source,每次重播多漏一个(默认上限 256,超了 prime_audio_queue 的 GenSources 断言 panic)。
    // 起点重新套用 set_current_time(播完时已归零,之后 setCurrentTime: 会覆盖),避免 current_packet
    // 停在末尾一灌就再次 EOF。
    if env.objc.borrow::<AVAudioPlayerHostObject>(this).finished {
        log_once!("[扫描修 2026-09-16] AVAudioPlayer 播完后再 play:立即停止旧队列,重新灌缓冲再播");
        log_dbg!("[(AVAudioPlayer*){:?} play] 播完后重新灌缓冲再播", this);
        let status = AudioQueueStop(env, aq_ref, true);
        assert_eq!(status, 0);
        let set_current_time = env.objc.borrow::<AVAudioPlayerHostObject>(this).set_current_time;
        () = msg![env; this setCurrentTime:set_current_time];
        let buffers = env.objc.borrow::<AVAudioPlayerHostObject>(this).audio_queue_buffers.unwrap();
        prime_player_buffers(env, this, aq_ref, buffers);
    }

    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).is_playing = true;

    let status = AudioQueueStart(env, aq_ref, Ptr::null());
    assert_eq!(status, 0);

    true
}

- (())pause {
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).is_playing = false;
    if let Some(aq_ref) = env.objc.borrow::<AVAudioPlayerHostObject>(this).audio_queue {
        AudioQueuePause(env, aq_ref);
    }
}

- (())stop {
    let &mut AVAudioPlayerHostObject {
        audio_queue,
        audio_queue_buffers,
        ..
    } = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this);
    if audio_queue.is_none() {
        // already being stopped
        return;
    }
    AudioQueueDispose(env, audio_queue.unwrap(), true);
    env.mem.free(audio_queue_buffers.unwrap().cast());

    let &AVAudioPlayerHostObject { audio_file_url, output_callback, num_of_loops, audio_file_id, delegate, .. } = env.objc.borrow(this);
    *env.objc.borrow_mut::<AVAudioPlayerHostObject>(this) = AVAudioPlayerHostObject {
        audio_file_url,
        output_callback,
        num_of_loops,
        audio_file_id,
        delegate,
        audio_desc: None,
        audio_queue: None,
        audio_queue_buffers: None,
        num_packets_to_read: 0,
        current_packet: 0,
        set_current_time: 0.0,
        volume: 1.0,
        is_playing: false,
        finished: false
    };
}

- (())setNumberOfLoops:(NSInteger)numberOfLoops {
    log_dbg!("[(AVAudioPlayer *) {:?} setNumberOfLoops:{:?}]", this, numberOfLoops);
    env.objc.borrow_mut::<AVAudioPlayerHostObject>(this).num_of_loops = numberOfLoops;
}

- (())dealloc {
    () = msg![env; this stop];
    let &AVAudioPlayerHostObject {audio_file_url, audio_file_id, ..} = env.objc.borrow(this);
    release(env, audio_file_url);
    if let Some(audio_file_id) = audio_file_id {
        AudioFileClose(env, audio_file_id);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

- (NSTimeInterval)currentTime {
    let host_object = env.objc.borrow::<AVAudioPlayerHostObject>(this);
    let current_time = if let Some(audio_desc) = host_object.audio_desc {
        let current_frame = (host_object.current_packet as f64) * (audio_desc.frames_per_packet as f64);
        current_frame / audio_desc.sample_rate
    } else {
        0.0
    };
    log_dbg!("[(AVAudioPlayer *) {:?} currentTime] -> {:?}", this, current_time);
    current_time
}
- (())setCurrentTime:(NSTimeInterval)currentTime {
    // TODO: Support setting current time before having an audio description
    let host_object = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this);
    host_object.set_current_time = currentTime;
    if let (Some(audio_desc), Some(audio_file_id)) = (host_object.audio_desc, host_object.audio_file_id) {
        let total_packets = audio_file::State::get(&mut env.framework_state).audio_files.get(&audio_file_id).unwrap().audio_file.packet_count();
        let total_frames = total_packets * audio_desc.frames_per_packet as u64;
        let new_current_frame = audio_desc.sample_rate * currentTime;
        if new_current_frame < 0.0 || new_current_frame > total_frames as f64 {
            host_object.current_packet = 0;
        } else {
            host_object.current_packet = (new_current_frame / (audio_desc.frames_per_packet as f64)) as i64;
        }
    }
    log_dbg!("[(AVAudioPlayer *) {:?} setCurrentTime: {}]", this, currentTime);
}

- (NSTimeInterval)duration {
    let host_object = env.objc.borrow::<AVAudioPlayerHostObject>(this);
    let audio_file_id = host_object.audio_file_id.unwrap();
    let audio_file = &audio_file::State::get(&mut env.framework_state)
        .audio_files
        .get(&audio_file_id)
        .unwrap()
        .audio_file;
    audio_file.estimated_duration()
}

@end

};

// Listing 3-7 from `Deriving a playback audio queue buffer size`
// from the Apple's guide
fn derive_buffer_size(
    audio_desc: AudioStreamBasicDescription,
    max_packet_size: u32,
    seconds: f64,
) -> (u32, u32) {
    let mut out_buffer_size;

    const max_buffer_size: u32 = 0x50000;
    const min_buffer_size: u32 = 0x4000;

    if audio_desc.frames_per_packet != 0 {
        let num_packets_to_time =
            audio_desc.sample_rate / audio_desc.frames_per_packet as f64 * seconds;
        out_buffer_size = num_packets_to_time as u32 * max_packet_size;
    } else {
        out_buffer_size = if max_buffer_size > max_packet_size {
            max_buffer_size
        } else {
            max_packet_size
        }
    }

    if out_buffer_size > max_buffer_size && out_buffer_size > max_packet_size {
        out_buffer_size = max_buffer_size
    } else if out_buffer_size < min_buffer_size {
        out_buffer_size = min_buffer_size
    }

    let out_num_packets_to_read = out_buffer_size / max_packet_size;
    (out_buffer_size, out_num_packets_to_read)
}

/// [扫描修 2026-09-16] 从 current_packet 起把已分配的 kNumberBuffers 个缓冲读满并入队,但不启动队列
/// (PlayingAudio 示例里的 "Priming the buffers"),prepareToPlay 首次准备和播完后重播共用。
/// 输出回调只在 is_playing 为真时读文件,所以期间临时置真,结束后复原为假,由 play 再置真。
fn prime_player_buffers(
    env: &mut Environment,
    this: id,
    aq_ref: AudioQueueRef,
    buffers: MutPtr<AudioQueueBufferRef>,
) {
    let host_object = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this);
    host_object.finished = false;
    host_object.is_playing = true;

    // 回调读到末尾且不循环时会置 finished。只有第一个缓冲就读到末尾(起点在末尾或空文件)才是
    // 一个包都没入队、真的已经播完;不足 3 个缓冲长的短文件在第 2、3 个缓冲读到末尾时,数据其实
    // 都已入队、还没开始播,不能算播完,否则 play 会白白重灌、isPlaying 也会错报为假。
    let mut nothing_enqueued = false;
    for i in 0..kNumberBuffers {
        let buffer = env.mem.read(buffers + i as u32);
        _touchHLE_AVAudioPlayerOutputBufferHelper(env, this.cast(), aq_ref, buffer);
        if i == 0 {
            nothing_enqueued = env.objc.borrow::<AVAudioPlayerHostObject>(this).finished;
        }
    }

    let host_object = env.objc.borrow_mut::<AVAudioPlayerHostObject>(this);
    host_object.is_playing = false;
    host_object.finished = nothing_enqueued;
}

/// (*void)(void *in_user_data, AudioQueueRef in_aq, AudioQueueBufferRef in_buf)
fn _touchHLE_AVAudioPlayerOutputBufferHelper(
    env: &mut Environment,
    in_user_data: MutVoidPtr,
    in_aq: AudioQueueRef,
    in_buf: AudioQueueBufferRef,
) {
    let av_audio_player: id = in_user_data.cast();
    let class: Class = msg![env; av_audio_player class];
    log_dbg!(
        "_touchHLE_AVAudioPlayerOutputBufferHelper on object of class: {}",
        env.objc.get_class_name(class)
    );
    assert_eq!(
        class,
        env.objc.get_known_class("AVAudioPlayer", &mut env.mem)
    );

    let &AVAudioPlayerHostObject {
        audio_file_id,
        audio_queue,
        num_packets_to_read,
        current_packet,
        is_playing,
        ..
    } = env.objc.borrow(av_audio_player);
    let aq = audio_queue.unwrap();
    assert_eq!(aq, in_aq);

    if !is_playing {
        return;
    }

    let num_bytes_ptr: MutPtr<u32> = env.mem.alloc(guest_size_of::<u32>()).cast();
    let num_packets_ptr: MutPtr<u32> = env.mem.alloc(guest_size_of::<u32>()).cast();
    env.mem.write(num_packets_ptr, num_packets_to_read);
    let mut audio_queue_buffer = env.mem.read(in_buf);
    let status = AudioFileReadPackets(
        env,
        audio_file_id.unwrap(),
        false,
        num_bytes_ptr,
        Ptr::null(),
        current_packet,
        num_packets_ptr,
        audio_queue_buffer.audio_data,
    );
    let num_packets = env.mem.read(num_packets_ptr);
    let num_bytes = env.mem.read(num_bytes_ptr);
    env.mem.free(num_packets_ptr.cast());
    env.mem.free(num_bytes_ptr.cast());

    if num_packets > 0 {
        assert!(status == 0 || status == eofErr);
        audio_queue_buffer.audio_data_byte_size = num_bytes;
        env.mem.write(in_buf, audio_queue_buffer);
        let status = AudioQueueEnqueueBuffer(env, aq, in_buf, 0, Ptr::null());
        assert_eq!(status, 0);
        env.objc
            .borrow_mut::<AVAudioPlayerHostObject>(av_audio_player)
            .current_packet = current_packet + num_packets as i64;
    } else {
        assert_eq!(status, eofErr);
        let number_of_loops = env
            .objc
            .borrow::<AVAudioPlayerHostObject>(av_audio_player)
            .num_of_loops;
        if number_of_loops == 0 {
            let status = AudioQueueStop(env, aq, false);
            assert_eq!(status, 0);
            let host_object = env
                .objc
                .borrow_mut::<AVAudioPlayerHostObject>(av_audio_player);
            host_object.is_playing = false;
            // [扫描修 2026-09-16] 记下已播完,下次 play 据此重新灌缓冲;iOS 上播完 currentTime 归零,
            // 下次从头播,所以重播起点也归零。
            host_object.finished = true;
            host_object.set_current_time = 0.0;
        } else {
            if number_of_loops > 0 {
                env.objc
                    .borrow_mut::<AVAudioPlayerHostObject>(av_audio_player)
                    .num_of_loops -= 1;
            }
            env.objc
                .borrow_mut::<AVAudioPlayerHostObject>(av_audio_player)
                .current_packet = 0;
            _touchHLE_AVAudioPlayerOutputBufferHelper(env, in_user_data, in_aq, in_buf);
        }
    }
}

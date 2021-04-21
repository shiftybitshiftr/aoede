use std::sync::Arc;
use tokio::sync::Mutex;

use librespot::core::authentication::Credentials;
use librespot::core::config::{ConnectConfig, DeviceType, SessionConfig, VolumeCtrl};
use librespot::core::session::Session;

use librespot::audio::AudioPacket;
use librespot::connect::spirc::Spirc;
use librespot::playback::audio_backend;
use librespot::playback::config::Bitrate;
use librespot::playback::config::PlayerConfig;
use librespot::playback::config::{NormalisationMethod, NormalisationType};
use librespot::playback::mixer::{AudioFilter, Mixer, MixerConfig};
use librespot::playback::player::{Player, PlayerEventChannel};
use serenity::prelude::TypeMapKey;
use std::clone::Clone;
use std::io;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use byteorder::ByteOrder;
use byteorder::LittleEndian;

pub struct SpotifyPlayer {
    player_config: PlayerConfig,
    pub emitted_sink: EmittedSink,
    session: Session,
    pub spirc: Option<Box<Spirc>>,
    pub event_channel: Option<Arc<Mutex<PlayerEventChannel>>>,
}

pub struct EmittedSink {
    sender: Arc<Mutex<SyncSender<u8>>>,
    pub receiver: Arc<Mutex<Receiver<u8>>>,
}

impl EmittedSink {
    fn new() -> EmittedSink {
        let (sender, receiver) = sync_channel::<u8>(24);

        EmittedSink {
            sender: Arc::new(Mutex::new(sender)),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}

struct ImpliedMixer {}

impl Mixer for ImpliedMixer {
    fn open(_config: Option<MixerConfig>) -> ImpliedMixer {
        ImpliedMixer {}
    }

    fn start(&self) {}

    fn stop(&self) {}

    fn volume(&self) -> u16 {
        50
    }

    fn set_volume(&self, _volume: u16) {}

    fn get_audio_filter(&self) -> Option<Box<dyn AudioFilter + Send>> {
        None
    }
}

impl audio_backend::Sink for EmittedSink {
    fn start(&mut self) -> std::result::Result<(), std::io::Error> {
        Ok(())
    }

    fn stop(&mut self) -> std::result::Result<(), std::io::Error> {
        Ok(())
    }

    #[tokio::main]
    async fn write(&mut self, packet: &AudioPacket) -> std::result::Result<(), std::io::Error> {
        let resampled = samplerate::convert(
            44100,
            48000,
            2,
            samplerate::ConverterType::Linear,
            packet.samples(),
        )
        .unwrap();

        let sender = self.sender.lock().await;

        for i in resampled {
            let mut new = [0, 0, 0, 0];

            LittleEndian::write_f32_into(&[i], &mut new);

            for j in new.iter() {
                sender.send(*j).unwrap();
            }
        }

        Ok(())
    }
}

impl io::Read for EmittedSink {
    #[tokio::main]
    async fn read(&mut self, buff: &mut [u8]) -> Result<usize, io::Error> {
        let receiver = self.receiver.lock().await;

        #[allow(clippy::needless_range_loop)]
        for i in 0..buff.len() {
            buff[i] = receiver.recv().unwrap();
        }

        Ok(buff.len())
    }
}

impl Clone for EmittedSink {
    fn clone(&self) -> EmittedSink {
        EmittedSink {
            receiver: self.receiver.clone(),
            sender: self.sender.clone(),
        }
    }
}

pub struct SpotifyPlayerKey;
impl TypeMapKey for SpotifyPlayerKey {
    type Value = Arc<Mutex<SpotifyPlayer>>;
}

impl Drop for SpotifyPlayer {
    fn drop(&mut self) {
        println!("dropping player");
    }
}

impl SpotifyPlayer {
    pub async fn new(
        username: String,
        password: String,
        quality: Bitrate,
        _cache_dir: String,
    ) -> SpotifyPlayer {
        let credentials = Credentials::with_password(username, password);

        let session_config = SessionConfig::default();

        let session = Session::connect(session_config, credentials, None)
            .await
            .expect("Error creating session");

        let player_config = PlayerConfig {
            bitrate: quality,
            normalisation: false,
            normalisation_type: NormalisationType::default(),
            normalisation_method: NormalisationMethod::default(),
            normalisation_pregain: 0.0,
            normalisation_threshold: -1.0,
            normalisation_attack: 0.005,
            normalisation_release: 0.1,
            normalisation_knee: 1.0,
            gapless: true,
            passthrough: false,
        };

        let emitted_sink = EmittedSink::new();

        let cloned_sink = emitted_sink.clone();

        let (_player, rx) = Player::new(player_config.clone(), session.clone(), None, move || {
            Box::new(cloned_sink)
        });

        SpotifyPlayer {
            player_config,
            emitted_sink,
            session,
            spirc: None,
            event_channel: Some(Arc::new(Mutex::new(rx))),
        }
    }

    pub async fn enable_connect(
        &mut self,
        device_name: String,
        device_type: DeviceType,
        initial_volume: u16,
        volume_ctrl: VolumeCtrl,
    ) {
        let config = ConnectConfig {
            name: device_name,
            device_type,
            volume: initial_volume,
            autoplay: true,
            volume_ctrl,
        };

        let mixer = Box::new(ImpliedMixer {});

        let cloned_sink = self.emitted_sink.clone();

        let (player, player_events) = Player::new(
            self.player_config.clone(),
            self.session.clone(),
            None,
            move || Box::new(cloned_sink),
        );

        let cloned_session = self.session.clone();

        let (spirc, task) = Spirc::new(config, cloned_session, player, mixer);

        let handle = tokio::runtime::Handle::current();
        handle.spawn(async {
            task.await;
        });

        self.spirc = Some(Box::new(spirc));

        let mut channel_lock = self.event_channel.as_ref().unwrap().lock().await;

        *channel_lock = player_events;
    }

    pub fn disable_connect(&mut self) {
        if let Some(spirc) = self.spirc.as_ref() {
            spirc.shutdown();
        }
    }
}

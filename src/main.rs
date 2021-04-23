use std::env;

use songbird::input;
use songbird::SerenityInit;

mod lib {
    pub mod player;
}
use lib::player::{SpotifyPlayer, SpotifyPlayerKey};
use librespot::core::mercury::MercuryError;
use librespot::playback::config::Bitrate;
use librespot::playback::player::PlayerEvent;
use std::sync::Arc;
use tokio::sync::Mutex;

use serenity::client::Context;

use serenity::prelude::TypeMapKey;

use serenity::{
    async_trait,
    client::{Client, EventHandler},
    framework::StandardFramework,
    model::{gateway, gateway::Ready, id, user, voice::VoiceState},
};

struct Handler;

pub struct UserIdKey;
impl TypeMapKey for UserIdKey {
    type Value = id::UserId;
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("Ready!");
        println!("Invite me with https://discord.com/api/oauth2/authorize?client_id={}&permissions=36700160&scope=bot", ready.user.id);

        let data = ctx.data.read().await;

        let player = data.get::<SpotifyPlayerKey>().unwrap().clone();
        let user_id = *data
            .get::<UserIdKey>()
            .expect("User ID placed in at initialisation.");

        // Get guild so it's cached
        let _guilds = ctx.cache.current_user().await.guilds(&ctx.http).await;

        let guild = match ctx.cache.guilds().await.first() {
            Some(guild_id) => match ctx.cache.guild(guild_id).await {
                Some(guild) => guild,
                None => panic!("Could not find guild."),
            },
            None => {
                panic!("Not currently in any guilds.");
            }
        };

        // Handle case when user is in VC when bot starts
        let channel_id = guild
            .voice_states
            .get(&user_id)
            .and_then(|voice_state| voice_state.channel_id);

        if channel_id.is_some() {
            // Enable casting
            player.lock().await.enable_connect().await;
        }

        let c = ctx.clone();

        // Spawn event channel handler for Spotify
        tokio::spawn(async move {
            loop {
                let channel = player.lock().await.event_channel.clone().unwrap();
                let mut receiver = channel.lock().await;

                let event = match receiver.recv().await {
                    Some(e) => e,
                    None => {
                        continue;
                    }
                };

                match event {
                    PlayerEvent::Stopped { .. } => {
                        c.set_presence(None, user::OnlineStatus::Online).await;

                        let manager = songbird::get(&c)
                            .await
                            .expect("Songbird Voice client placed in at initialisation.")
                            .clone();

                        let _ = manager.leave(guild.id).await;
                    }

                    PlayerEvent::Started { .. } => {
                        let manager = songbird::get(&c)
                            .await
                            .expect("Songbird Voice client placed in at initialisation.")
                            .clone();

                        let channel_id = match guild
                            .voice_states
                            .get(&user_id)
                            .and_then(|voice_state| voice_state.channel_id)
                        {
                            Some(channel_id) => channel_id,
                            None => {
                                continue;
                            }
                        };

                        let _handler = manager.join(guild.id, channel_id).await;

                        if let Some(handler_lock) = manager.get(guild.id) {
                            let mut handler = handler_lock.lock().await;

                            let mut decoder = input::codec::OpusDecoderState::new().unwrap();
                            decoder.allow_passthrough = false;

                            let source = input::Input::new(
                                true,
                                input::reader::Reader::Extension(Box::new(
                                    player.lock().await.emitted_sink.clone(),
                                )),
                                input::codec::Codec::FloatPcm,
                                input::Container::Raw,
                                None,
                            );

                            handler.set_bitrate(songbird::Bitrate::Auto);

                            handler.play_source(source);
                        }
                    }

                    PlayerEvent::Paused { .. } => {
                        c.set_presence(None, user::OnlineStatus::Online).await;
                    }

                    PlayerEvent::Playing { track_id, .. } => {
                        let track: Result<librespot::metadata::Track, MercuryError> =
                            librespot::metadata::Metadata::get(
                                &player.lock().await.session,
                                track_id,
                            )
                            .await;

                        if let Ok(track) = track {
                            let artist: Result<librespot::metadata::Artist, MercuryError> =
                                librespot::metadata::Metadata::get(
                                    &player.lock().await.session,
                                    *track.artists.first().unwrap(),
                                )
                                .await;

                            if let Ok(artist) = artist {
                                let listening_to = format!("{}: {}", artist.name, track.name);

                                c.set_presence(
                                    Some(gateway::Activity::listening(listening_to)),
                                    user::OnlineStatus::Online,
                                )
                                .await;
                            }
                        }
                    }

                    _ => {}
                }
            }
        });
    }

    async fn voice_state_update(
        &self,
        ctx: Context,
        _: Option<id::GuildId>,
        old: Option<VoiceState>,
        new: VoiceState,
    ) {
        let data = ctx.data.read().await;

        let user_id = data.get::<UserIdKey>();

        if new.user_id.to_string() != user_id.unwrap().to_string() {
            return;
        }

        let player = data.get::<SpotifyPlayerKey>().unwrap();

        // If user just connected
        if old.clone().is_none() {
            // Enable casting
            player.lock().await.enable_connect().await;
            return;
        }

        // If user disconnected
        if old.clone().unwrap().channel_id.is_some() && new.channel_id.is_none() {
            // Disable casting
            player.lock().await.disable_connect();
            return;
        }

        // If user moved channels
        if old.unwrap().channel_id.unwrap() != new.channel_id.unwrap() {
            let manager = songbird::get(&ctx)
                .await
                .expect("Songbird Voice client placed in at initialisation.")
                .clone();

            if let Some(guild_id) = ctx.cache.guilds().await.first() {
                let _handler = manager.join(*guild_id, new.channel_id.unwrap()).await;
            }

            return;
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

    let framework = StandardFramework::new();
    let username =
        env::var("SPOTIFY_USERNAME").expect("Expected a Spotify username in the environment");
    let password =
        env::var("SPOTIFY_PASSWORD").expect("Expected a Spotify password in the environment");
    let user_id =
        env::var("DISCORD_USER_ID").expect("Expected a Discord user ID in the environment");

    let mut cache_dir = None;

    if let Ok(c) = env::var("CACHE_DIR") {
        cache_dir = Some(c);
    }

    let player = Arc::new(Mutex::new(
        SpotifyPlayer::new(username, password, Bitrate::Bitrate320, cache_dir).await,
    ));

    let mut client = Client::builder(&token)
        .event_handler(Handler)
        .framework(framework)
        .type_map_insert::<SpotifyPlayerKey>(player)
        .type_map_insert::<UserIdKey>(id::UserId::from(user_id.parse::<u64>().unwrap()))
        .register_songbird()
        .await
        .expect("Err creating client");

    let _ = client
        .start()
        .await
        .map_err(|why| println!("Client ended: {:?}", why));
}

use async_trait::async_trait;
use futures::TryStreamExt;
use serde::{Serialize, Serializer};
use serde_json::json;

use iota_streams::app::message::LinkedMessage;
use iota_streams::app::transport::tangle::{TangleAddress, TangleMessage};
use iota_streams::app::transport::Transport;
use iota_streams::app_channels::{Author, ChannelType, Subscriber};

type Result<T> = std::result::Result<T, anyhow::Error>;

#[tokio::main]
async fn main() -> Result<()> {
    let mut immudb_client = ImmuDB::new();
    immudb_client.login().await?;
    let author_seed = "author seed 2";
    let mut author = Author::new(author_seed, ChannelType::MultiBranch, immudb_client.clone());
    println!("Created Author {author}");
    let announcement_link = author.announcement_link().unwrap();
    let is_new = author.send_announce().await.is_ok();
    if !is_new {
        author = Author::recover(
            author_seed,
            &announcement_link,
            ChannelType::MultiBranch,
            immudb_client.clone(),
        )
        .await?;
    }
    println!("Announcement link: {announcement_link}");
    println!("freshly created: {is_new}");
    let num_msgs = author.sync_state().await?;
    println!("Synchronized {num_msgs} messages");
    let mut subscriber = Subscriber::new("subscriber seed", immudb_client);
    println!("Created Subscriber {subscriber}");
    subscriber.receive_announcement(&announcement_link).await?;
    println!("Subscriber received announcement");
    let subscription_link = subscriber.send_subscribe(&announcement_link).await?;
    println!("Subscriber sent subscription {subscription_link}");
    let subscriber_is_new = author.receive_subscribe(&subscription_link).await.is_ok();
    println!("Author received subscription");
    println!("Subscriber is new: {subscriber_is_new}");
    let (keyload_link, _) = author.send_keyload_for_everyone(&announcement_link).await?;
    println!("Author sent keyload {keyload_link}");
    let (mut last_msg_link, _) = author
        .send_signed_packet(&keyload_link, &b"".into(), &b"test branch".into())
        .await?;
    println!("Author sent signed packet {last_msg_link}");
    for x in 0u8..100 {
        let (msg_link, _) = author
            .send_signed_packet(&last_msg_link, &b"".into(), &x.to_ne_bytes().into())
            .await?;
        last_msg_link = msg_link;
    }
    println!("Author sent 100 other messages");
    let mut messages = subscriber.messages();
    let empty_payload = b"empty".into();
    while let Some(msg) = messages.try_next().await? {
        let payload = msg.body.masked_payload().unwrap_or(&empty_payload);
        println!("Subscriber received masked payload {payload}")
    }
    Ok(())
}

#[derive(Clone)]
struct DummyTransport();

impl DummyTransport {
    #[allow(unused)]
    fn new() -> Self {
        Self()
    }
}

#[async_trait(?Send)]
impl Transport<TangleAddress, TangleMessage> for DummyTransport {
    async fn send_message(&mut self, _msg: &TangleMessage) -> iota_streams::core::Result<()> {
        todo!()
    }

    async fn recv_message(
        &mut self,
        _link: &TangleAddress,
    ) -> iota_streams::core::Result<TangleMessage> {
        todo!()
    }
}

#[derive(Clone, Debug)]
struct ImmuDB {
    client: reqwest::Client,
}

impl ImmuDB {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    async fn login(&mut self) -> Result<()> {
        #[derive(Serialize)]
        struct TokenRequest {
            #[serde(serialize_with = "serialize_to_base64")]
            user: String,
            #[serde(serialize_with = "serialize_to_base64")]
            password: String,
        }
        self.client
            .post(self.build_url("/login"))
            .json(&TokenRequest {
                user: String::from("immudb"),
                password: String::from("immudb"),
            })
            .send()
            .await?;
        self.client
            .get(self.build_url("/db/use/defaultdb"))
            .send()
            .await?;

        Ok(())
    }

    fn build_url(&self, path: &str) -> String {
        let domain = "http://127.0.0.1:3323";
        format!("{domain}{path}")
    }
}

fn serialize_to_base64<S>(string: &String, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&base64::encode(string))
}

#[async_trait(?Send)]
impl Transport<TangleAddress, TangleMessage> for ImmuDB {
    async fn send_message(&mut self, msg: &TangleMessage) -> iota_streams::core::Result<()> {
        let msg_index = msg.link().to_msg_index();
        let response = self
            .client
            .post(self.build_url("/db/verified/set"))
            .json(&json!({"setRequest": {"KVs": [
                {
                  "key": base64::encode(&msg_index),
                     "value": base64::encode(&msg.body)
                }
              ]
            }}))
            .send()
            .await?;
        // println!("send response: {response}");
        if response.status().is_success() {
            Ok(())
        } else {
            anyhow::bail!("error sending message to {msg_index}");
        }
    }

    async fn recv_message(
        &mut self,
        link: &TangleAddress,
    ) -> iota_streams::core::Result<TangleMessage> {
        let response = self
            .client
            .post(self.build_url("/db/verified/get"))
            .json(&json!({
                "keyRequest": {
                  "key": base64::encode(link.to_msg_index())
                }
            }))
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            let payload: serde_json::Value = response.json().await?;
            // println!("recv response: {payload}");
            Ok(TangleMessage::new(
                *link,
                TangleAddress::default(),
                base64::decode(
                    payload["value"]
                        .as_str()
                        .expect("getting message from successful recv response"),
                )?
                .into(),
            ))
        } else {
            // println!("recv error response: {}", response.text().await?);
            anyhow::bail!("Error ({status})")
        }
    }
}

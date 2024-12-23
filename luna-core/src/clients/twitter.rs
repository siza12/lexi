use crate::{
agent::Agent,
attention::{Attention, AttentionCommand, AttentionContext},
knowledge::{ChannelType, Message, Source},
};

use rig::{
completion::{CompletionModel, Prompt},
embeddings::EmbeddingModel,
};
use std::collections::HashSet;
use tracing::{debug, error, info};
use twitter::{authorization::Authorization, TwitterApi};
use twitter_v2::{self as twitter, authorization::{BearerToken, Oauth1aToken}};
use twitter_v2::data::ReferencedTweetKind;

const MAX_TWEET_LENGTH: usize = 280;
const MAX_HISTORY_TWEETS: i64 = 10;

#[derive(Clone)]
pub struct TwitterClient<M: CompletionModel, E: EmbeddingModel + 'static, A: Authorization> {
agent: Agent<M, E>,
attention: Attention<M>,
api: TwitterApi<A>,
}

impl Fromtwitter::Tweet for Message {
fn from(tweet: twitter::Tweet) -> Self {
let created_at = tweet
.created_at
.map(|t| chrono::DateTime::from_timestamp(t.unix_timestamp(), 0).unwrap_or_default())
.unwrap_or_default();

```
    Self {
        id: tweet.id.to_string(),
        source: Source::Twitter,
        source_id: tweet.id.to_string(),
        channel_type: ChannelType::Text,
        channel_id: tweet.conversation_id.unwrap_or(tweet.id).to_string(),
        account_id: tweet
            .author_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "0".to_string()),
        role: "user".to_string(),
        content: tweet.text.clone(),
        created_at,
    }
}

```

}

impl<M: CompletionModel + 'static, E: EmbeddingModel + 'static> TwitterClient<M, E, Oauth1aToken> {
pub fn new(agent: Agent<M, E>, attention: Attention<M>, oauth1a_token: Oauth1aToken) -> Self {
let api = TwitterApi::new(oauth1a_token);

```
    Self {
        agent,
        attention,
        api,
    }
}

```

}

impl<M: CompletionModel + 'static, E: EmbeddingModel + 'static> TwitterClient<M, E, BearerToken> {
pub fn new(agent: Agent<M, E>, attention: Attention<M>, bearer_token: &str) -> Self {
let auth = BearerToken::new(bearer_token.to_string());
let api = TwitterApi::new(auth);

```
    Self {
        agent,
        attention,
        api,
    }
}

```

}

impl<M: CompletionModel + 'static, E: EmbeddingModel + 'static, A: Authorization> TwitterClient<M, E, A> {
pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
info!("Starting Twitter bot");
self.listen_for_mentions().await
}

```
async fn listen_for_mentions(&self) -> Result<(), Box<dyn std::error::Error>> {
    let me = self.api.get_users_me().send().await?;
    let user_id = me.data.as_ref().unwrap().id;

    // In a real implementation, you would use Twitter's streaming API
    // This is a simplified polling approach
    loop {
        let mentions = self
            .api
            .get_user_mentions(user_id)
            .max_results(5)
            .send()
            .await?;

        for tweet in mentions.data.clone().unwrap_or_default() {
            self.handle_mention(tweet).await?;
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

async fn handle_mention(
    &self,
    tweet: twitter::Tweet,
) -> Result<(), Box<dyn std::error::Error>> {
    let knowledge = self.agent.knowledge();
    let knowledge_msg = Message::from(tweet.clone());

    if let Err(err) = knowledge.create_message(knowledge_msg.clone()).await {
        error!(?err, "Failed to store tweet");
        return Ok(());
    }

    let thread = self.build_conversation_thread(&tweet).await?;

    let mentioned_names: HashSet<String> = tweet
        .text
        .split_whitespace()
        .filter(|word| word.starts_with('@'))
        .map(|mention| mention[1..].to_string())
        .collect();

    debug!(
        mentioned_names = ?mentioned_names,
        "Mentioned names in tweet"
    );

    let history = thread
        .iter()
        .map(|t| (t.id.to_string(), t.text.clone()))
        .collect();

    let context = AttentionContext {
        message_content: tweet.text.clone(),
        mentioned_names,
        history,
        channel_type: knowledge_msg.channel_type,
        source: knowledge_msg.source,
    };

    debug!(?context, "Attention context");

    match self.attention.should_reply(&context).await {
        AttentionCommand::Respond => {}
        _ => {
            debug!("Bot decided not to reply to tweet");
            return Ok(());
        }
    }

    let agent = self
        .agent
        .builder()
        .context(&format!(
            "Current time: {}",
            chrono::Local::now().format("%I:%M:%S %p, %Y-%m-%d")
        ))
        .context("Please keep your responses concise and under 280 characters.")
        .build();

    let response = match agent.prompt(&tweet.text).await {
        Ok(response) => response,
        Err(err) => {
            error!(?err, "Failed to generate response");
            return Ok(());
        }
    };

    debug!(response = %response, "Generated response");

    // Split response into tweet-sized chunks if necessary
    let chunks: Vec<String> = response
        .chars()
        .collect::<Vec<char>>()
        .chunks(MAX_TWEET_LENGTH)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect();

    // Reply to the original tweet
    for chunk in chunks {
        if let Err(err) = self
            .api
            .post_tweet()
            .in_reply_to_tweet_id(tweet.id)
            .text(chunk)
            .send()
            .await
        {
            error!(?err, "Failed to send tweet");
        }
    }

    Ok(())
}

async fn build_conversation_thread(
    &self,
    tweet: &twitter::Tweet,
) -> Result<Vec<twitter::Tweet>, Box<dyn std::error::Error>> {
    let mut thread = Vec::new();
    let mut current_tweet = Some(tweet.clone());
    let mut depth = 0;

    while let Some(tweet) = current_tweet {
        thread.push(tweet.clone());

        if depth >= MAX_HISTORY_TWEETS {
            break;
        }

        if let Some(referenced_tweets) = &tweet.referenced_tweets {
            if let Some(replied_to) = referenced_tweets
                .iter()
                .find(|t| matches!(t.kind, ReferencedTweetKind::RepliedTo))
            {
                match self.api.get_tweet(replied_to.id).send().await {
                    Ok(response) => {
                        current_tweet = response.data.clone();
                    }
                    Err(err) => {
                        error!(?err, "Failed to fetch parent tweet");
                        break;
                    }
                }
            } else {
                break;
            }
        } else {
            break;
        }

        depth += 1;
    }

    thread.reverse(); // Order from oldest to newest
    Ok(thread)
}

```

}

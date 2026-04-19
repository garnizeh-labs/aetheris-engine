use async_trait::async_trait;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::{debug, error, info};

#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(
        &self,
        to: &str,
        subject: &str,
        plaintext: &str,
        html: &str,
    ) -> Result<(), String>;
}

pub struct LogEmailSender;

#[async_trait]
impl EmailSender for LogEmailSender {
    async fn send(
        &self,
        to: &str,
        subject: &str,
        plaintext: &str,
        _html: &str,
    ) -> Result<(), String> {
        info!("Sending email to: {} Subject: {}", to, subject);
        debug!("Body: {}", plaintext);
        Ok(())
    }
}

pub struct LettreSmtpEmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
}

impl LettreSmtpEmailSender {
    pub fn from_env() -> Result<Self, String> {
        let smtp_url = std::env::var("SMTP_URL").map_err(|_| "SMTP_URL missing")?;
        let smtp_user = std::env::var("SMTP_USERNAME").map_err(|_| "SMTP_USERNAME missing")?;
        let smtp_pass = std::env::var("SMTP_PASSWORD").map_err(|_| "SMTP_PASSWORD missing")?;
        let from = std::env::var("SMTP_FROM").map_err(|_| "SMTP_FROM missing")?;

        let creds = Credentials::new(smtp_user, smtp_pass);
        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_url)
            .map_err(|e| e.to_string())?
            .credentials(creds)
            .build();

        Ok(Self { transport, from })
    }
}

#[async_trait]
impl EmailSender for LettreSmtpEmailSender {
    async fn send(
        &self,
        to: &str,
        subject: &str,
        plaintext: &str,
        html: &str,
    ) -> Result<(), String> {
        let email = Message::builder()
            .from(
                self.from
                    .parse()
                    .map_err(|e: lettre::address::AddressError| e.to_string())?,
            )
            .to(to
                .parse()
                .map_err(|e: lettre::address::AddressError| e.to_string())?)
            .subject(subject)
            .multipart(
                lettre::message::MultiPart::alternative()
                    .singlepart(lettre::message::SinglePart::plain(plaintext.to_string()))
                    .singlepart(lettre::message::SinglePart::html(html.to_string())),
            )
            .map_err(|e| e.to_string())?;

        let _ = self.transport.send(email).await.map_err(|e| {
            error!("Failed to send email: {}", e);
            e.to_string()
        })?;
        Ok(())
    }
}
pub struct ResendEmailSender {
    client: reqwest::Client,
    api_key: String,
    from: String,
}

impl ResendEmailSender {
    #[must_use]
    pub fn new(api_key: String, from: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(5))
            .pool_max_idle_per_host(2)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            api_key,
            from,
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("RESEND_API_KEY").map_err(|_| "RESEND_API_KEY missing")?;
        let from =
            std::env::var("RESEND_FROM").unwrap_or_else(|_| "onboarding@resend.dev".to_string());
        Ok(Self::new(api_key, from))
    }
}

#[async_trait]
impl EmailSender for ResendEmailSender {
    async fn send(
        &self,
        to: &str,
        subject: &str,
        plaintext: &str,
        html: &str,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "from": self.from,
            "to": [to],
            "subject": subject,
            "text": plaintext,
            "html": html,
        });

        let response = self
            .client
            .post("https://api.resend.com/emails")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            error!("Resend API error ({status}): {error_text}");
            Err(format!("Resend API error ({status}): {error_text}"))
        }
    }
}

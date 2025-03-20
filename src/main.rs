use clap::Parser;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use nostr_sdk::{
    Client, Event, EventBuilder, Keys, Kind, PublicKey, RelayPoolNotification, Tag,
    SecretKey, Timestamp,
};
use nostr_sdk::prelude::*;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout},
    style::{Color, Style},
    style::palette::tailwind::{BLUE},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    prelude::Stylize, 
    Terminal,
};
use std::{
    io,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc::{self, Receiver, Sender};

// Relay URL
const RELAY_URL: &str = "wss://relay.mostro.network";

// We set N in seconds (600 seconds = 10 minutes)
const N_SECONDS: u64 = 600;
// Proof of work difficulty is important for the NIP-59 event
const POW_DIFFICULTY: u8 = 2;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Sender's private key (hex or bech32)
    #[arg(short = 's', long = "sender-secret", requires = "receiver_pubkey")]
    sender_secret: Option<String>,

    /// Receiver's public key (hex or bech32)
    #[arg(short = 'p', long = "receiver-pubkey", requires = "sender_secret")]
    receiver_pubkey: Option<String>,

    /// Shared secret key (hex)
    #[arg(short = 'k', long = "shared-key", conflicts_with_all = ["sender_secret", "receiver_pubkey"])]
    shared_key: Option<String>,
}

struct App {
    messages: Arc<Mutex<Vec<(Timestamp, PublicKey, String)>>>,
    input: String,
    tx: Option<Sender<String>>,
    sender_keys: Option<Keys>,
    shared_keys: Keys,
    shared_key_display: String,
    is_observer: bool,
    is_shared_key_visible: bool,
    selected_tab: usize,
}

impl App {
    fn new(
        messages: Arc<Mutex<Vec<(Timestamp, PublicKey, String)>>>,
        tx: Option<Sender<String>>,
        sender_keys: Option<Keys>,
        shared_keys: Keys,
        is_observer: bool,
    ) -> Self {
        let shared_key_display = shared_keys.secret_key().to_secret_hex();
        Self {
            messages,
            input: String::new(),
            tx,
            sender_keys,
            shared_keys,
            shared_key_display,
            is_observer,
            is_shared_key_visible: false,
            selected_tab: 0,
        }
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();

    let shared_keys: Keys;
    let mut sender_keys: Option<Keys> = None;
    let is_observer = args.shared_key.is_some();

    if let Some(shared_key_hex) = args.shared_key {
        let shared_secret_key = SecretKey::from_str(&shared_key_hex).expect("Invalid shared key");
        shared_keys = Keys::new(shared_secret_key);
    } else {
        let sender_secret = args.sender_secret.expect("Sender secret is required");
        let receiver_pubkey_str = args.receiver_pubkey.expect("Receiver pubkey is required");

        sender_keys = Some(Keys::parse(&sender_secret).expect("Invalid sender's private key"));
        let receiver_pubkey = PublicKey::from_str(&receiver_pubkey_str).expect("Invalid recipient public key");

        let shared_key = nostr_sdk::util::generate_shared_key(
            sender_keys.as_ref().unwrap().secret_key(),
            &receiver_pubkey,
        ).expect("Error generating shared key");
        let shared_secret_key = SecretKey::from_slice(&shared_key).unwrap();
        shared_keys = Keys::new(shared_secret_key);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, rx) = if !is_observer {
        let (tx, rx) = mpsc::channel(100);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let messages = Arc::new(Mutex::new(Vec::new()));

    let app = App::new(messages.clone(), tx, sender_keys.clone(), shared_keys.clone(), is_observer);
    let nostr_handle = tokio::spawn(run_nostr(sender_keys, shared_keys, rx, messages.clone(), is_observer));

    let result = run_app(&mut terminal, app).await;

    nostr_handle.abort();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| {
            let vertical = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]);
            let [tabs_area, body_area] = vertical.areas(f.area());

            let tab_titles = ["Messages", "Keys"]
                .iter()
                .map(|t| Line::from(*t).bold())
                .collect::<Vec<Line>>();

            let tabs = Tabs::new(tab_titles)
                .block(Block::bordered()
                    .title_top(Line::from(" Mostro-Chat ").alignment(Alignment::Left))
                    .title_top(Line::from(Span::raw("Esc to exit").style(Style::new().fg(Color::Green))).alignment(Alignment::Right))
                )
                .select(app.selected_tab)
                .highlight_style(Style::new().fg(BLUE.c400));

            f.render_widget(tabs, tabs_area);

           

            match app.selected_tab {
                0 => {
                    let chunks = Layout::vertical([
                        Constraint::Percentage(80),
                        Constraint::Percentage(20),
                    ])
                    .split(body_area);

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("Error getting current time")
                        .as_secs();

                    let messages = app.messages.lock().expect("Error locking messages");
                    let recent_messages: Vec<String> = messages
                        .iter()
                        .filter(|(timestamp, _, _)| now - timestamp.as_u64() <= N_SECONDS)
                        .map(|(_, pubkey, msg)| {
                            if app.is_observer {
                                format!("{}: {}", pubkey.to_string(), msg)
                            } else {
                                if Some(*pubkey) == app.sender_keys.as_ref().map(|k| k.public_key()) {
                                    format!("You: {}", msg)
                                } else {
                                    format!("{}: {}", pubkey.to_string(), msg)
                                }
                            }
                        })
                        .collect();

                    let lines: Vec<Line> = recent_messages
                        .iter()
                        .map(|msg| Line::from(Span::raw(msg)))
                        .collect();
                    let messages_widget = Paragraph::new(lines)
                        .block(
                            Block::default()
                                .title_top(Line::from("Messages").alignment(Alignment::Left))
                                .title_style(Style::new().fg(BLUE.c400))
                                .borders(Borders::ALL)
                        );
                    f.render_widget(messages_widget, chunks[0]);

                    let input = Paragraph::new(app.input.as_str())
                        .style(Style::default().fg(Color::Yellow))
                        .block(Block::default().title("Input").borders(Borders::ALL));
                    f.render_widget(input, chunks[1]);
                }
                1 => {
                    let chunks = Layout::vertical([
                        Constraint::Percentage(50),
                        Constraint::Percentage(50),
                    ])
                    .split(body_area);

                    let shared_key_text = if app.is_shared_key_visible {
                        app.shared_key_display.as_str()
                    } else {
                        &"*".repeat(64)
                    };

                    let shared_key_widget = Paragraph::new(shared_key_text)
                        .style(Style::default().fg(Color::Gray))
                        .block(
                            Block::default()
                                .title("Shared Private Key")
                                .title_style(Style::new().fg(BLUE.c400))
                                .title_top(Line::from(Span::styled("Tab to Display/Hide", Style::default().fg(Color::Green))).alignment(Alignment::Right))
                                .borders(Borders::ALL)
                        );
                    f.render_widget(shared_key_widget, chunks[0]);

                    let shared_public_key_text = app.shared_keys.public_key().to_string();
                    let shared_public_key_widget = Paragraph::new(shared_public_key_text)
                        .style(Style::default().fg(Color::Gray))
                        .block(
                            Block::default()
                                .title("Shared Public Key")
                                .title_style(Style::new().fg(BLUE.c400))
                                .borders(Borders::ALL)
                        );
                    f.render_widget(shared_public_key_widget, chunks[1]);
                }
                _ => {}
            }
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let CrosstermEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Esc => break,
                    KeyCode::Char(c) => {
                        if app.selected_tab == 0 {
                            app.input.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if app.selected_tab == 0 {
                            app.input.pop();
                        }
                    }
                    KeyCode::Enter => {
                        if app.selected_tab == 0 && !app.input.is_empty() {
                            let message = app.input.clone();
                            let now = Timestamp::now();
                            let sender_pubkey = app.sender_keys.as_ref().map(|k| k.public_key()).unwrap_or(app.shared_keys.public_key());
                            {
                                let mut messages = app.messages.lock().expect("Error locking messages");
                                messages.push((now, sender_pubkey, message.clone()));
                            }
                            if let Some(tx) = &app.tx {
                                if let Err(e) = tx.send(message).await {
                                    let mut messages = app.messages.lock().expect("Error locking messages");
                                    messages.push((
                                        Timestamp::now(),
                                        sender_pubkey,
                                        format!("Error sending message: {}", e),
                                    ));
                                }
                            }
                            app.input.clear();
                        }
                    }
                    KeyCode::Tab => {
                        app.is_shared_key_visible = !app.is_shared_key_visible;
                    }
                    KeyCode::Left => {
                        if app.selected_tab > 0 {
                            app.selected_tab -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if app.selected_tab < 1 {
                            app.selected_tab += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn run_nostr(
    sender: Option<Keys>,
    shared_keys: Keys,
    rx: Option<Receiver<String>>,
    messages: Arc<Mutex<Vec<(Timestamp, PublicKey, String)>>>,
    is_observer: bool,
) {
    let client = Client::new(Keys::generate());
    if let Err(e) = client.add_relay(RELAY_URL).await {
        eprintln!("Error adding relay: {}", e);
        return;
    }
    let _ = client.connect().await;

    let filter = nostr_sdk::Filter::new()
        .kind(Kind::GiftWrap)
        .pubkey(shared_keys.public_key());
    if let Err(e) = client.subscribe(filter, None).await {
        eprintln!("Error subscribing: {}", e);
        return;
    }

    if !is_observer {
        let client_clone = client.clone();
        let receiver_clone = shared_keys.clone();
        let sender = sender.expect("Sender keys are required in participant mode");
        if let Some(mut rx) = rx {
            tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    if let Err(e) = send_message(&client_clone, &sender, receiver_clone.public_key(), &message).await {
                        eprintln!("Error sending message: {}", e);
                    }
                }
            });
        }
    }

    let mut notifications = client.notifications();
    while let Ok(notification) = notifications.recv().await {
        if let RelayPoolNotification::Event { event, .. } = notification {
            if let Ok(inner_event) = mostro_unwrap(&shared_keys, *event).await {
                let message = inner_event.content.clone();
                let created_at = inner_event.created_at;
                let sender_pubkey = inner_event.pubkey;
                let mut messages = messages.lock().expect("Error locking messages");
                messages.push((created_at, sender_pubkey, message));
            }
        }
    }
}

async fn send_message(
    client: &Client,
    sender: &Keys,
    receiver: PublicKey,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let wrapped_event = mostro_wrap(sender, receiver, message, vec![]).await?;
    client.send_event(&wrapped_event).await?;
    Ok(())
}

pub async fn mostro_wrap(
    sender: &Keys,
    receiver: PublicKey,
    message: &str,
    extra_tags: Vec<Tag>,
) -> Result<Event, Box<dyn std::error::Error>> {
    let inner_event = EventBuilder::text_note(message)
        .build(sender.public_key())
        .sign(sender)
        .await?;
    let keys: Keys = Keys::generate();
    let encrypted_content: String = nip44::encrypt(
        keys.secret_key(),
        &receiver,
        inner_event.as_json(),
        nip44::Version::V2,
    )
    .unwrap();

    let mut tags = vec![Tag::public_key(receiver)];
    tags.extend(extra_tags);

    let wrapped_event = EventBuilder::new(Kind::GiftWrap, encrypted_content)
        .pow(POW_DIFFICULTY)
        .tags(tags)
        .custom_created_at(Timestamp::tweaked(nip59::RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(&keys)?;
    Ok(wrapped_event)
}

pub async fn mostro_unwrap(
    receiver: &Keys,
    event: Event,
) -> Result<Event, Box<dyn std::error::Error>> {
    let decrypted_content = nip44::decrypt(receiver.secret_key(), &event.pubkey, &event.content)?;
    let inner_event = Event::from_json(&decrypted_content)?;

    inner_event.verify()?;

    Ok(inner_event)
}
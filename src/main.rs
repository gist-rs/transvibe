use color_eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use futures_util::StreamExt; // Removed TryStreamExt
use kalosm::language::*;
use kalosm::sound::*;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use tokio::sync::mpsc;

#[derive(Debug)]
enum AppUpdate {
    LiveJapaneseUpdate(String),
    JapaneseSegmentComplete(String),
    EnglishTranslation(String),
    StatusUpdate(String),
    Error(String),
}

struct App {
    status: String,
    current_live_japanese: String,
    completed_japanese: Vec<String>,
    completed_translations: Vec<String>,
    rx: mpsc::Receiver<AppUpdate>,
    should_quit: bool,
}

impl App {
    fn new(rx: mpsc::Receiver<AppUpdate>) -> Self {
        Self {
            status: "Initializing...".to_string(),
            current_live_japanese: String::new(),
            completed_japanese: Vec::new(),
            completed_translations: Vec::new(),
            rx,
            should_quit: false,
        }
    }

    fn on_update(&mut self, update: AppUpdate) {
        match update {
            AppUpdate::StatusUpdate(s) => self.status = s,
            AppUpdate::LiveJapaneseUpdate(s) => self.current_live_japanese = s,
            AppUpdate::JapaneseSegmentComplete(jp_text) => {
                self.completed_japanese.push(jp_text);
                self.current_live_japanese.clear();
                // Keep lists of the same length, add placeholder if translation is pending
                if self.completed_translations.len() < self.completed_japanese.len() {
                    self.completed_translations
                        .push("Translating...".to_string());
                }
            }
            AppUpdate::EnglishTranslation(en_text) => {
                if self.completed_translations.len() == self.completed_japanese.len() - 1 {
                    // This is the translation for the last Japanese segment
                    if let Some(last_translation) = self.completed_translations.last_mut() {
                        if last_translation == "Translating..." {
                            *last_translation = en_text;
                        } else {
                            // This case should ideally not happen if logic is correct
                            // but as a fallback, push it.
                            self.completed_translations.push(en_text);
                        }
                    } else {
                        // If completed_translations was empty but japanese wasn't (e.g. first item)
                        self.completed_translations.push(en_text);
                    }
                } else if self.completed_translations.len() < self.completed_japanese.len() {
                    // If there's a mismatch, try to update the last "Translating..." or append
                    if let Some(last_translation) = self.completed_translations.last_mut() {
                        if last_translation == "Translating..." {
                            *last_translation = en_text;
                        } else {
                            self.completed_translations.push(en_text);
                        }
                    } else {
                        self.completed_translations.push(en_text);
                    }
                } else {
                    // If lengths match or translation is ahead (should not happen), just push.
                    // This might indicate a logic issue or rapid updates.
                    self.completed_translations.push(en_text);
                }

                // Ensure translation list doesn't exceed Japanese list
                while self.completed_translations.len() > self.completed_japanese.len() {
                    self.completed_translations.pop();
                }
            }
            AppUpdate::Error(e) => {
                self.status = format!("ERROR: {}", e);
                // Potentially log to a file or display more prominently
            }
        }
    }

    fn run(mut self, mut terminal: Terminal<impl Backend>) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;
            self.handle_events()?;
            self.handle_updates();
        }
        Ok(())
    }

    fn handle_events(&mut self) -> Result<()> {
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if key.code == KeyCode::Char('q') {
                        self.should_quit = true;
                        self.status = "Exiting...".to_string();
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_updates(&mut self) {
        while let Ok(update) = self.rx.try_recv() {
            self.on_update(update);
        }
    }

    fn render(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![
                Constraint::Length(1), // Status
                Constraint::Length(3), // Live Japanese
                Constraint::Min(0),    // History
            ])
            .split(frame.size());

        let status_paragraph =
            Paragraph::new(self.status.as_str()).style(Style::default().fg(Color::Yellow));
        frame.render_widget(status_paragraph, main_layout[0]);

        let live_japanese_block = Block::default()
            .title("Live Japanese Input")
            .borders(Borders::ALL);
        let live_japanese_paragraph = Paragraph::new(self.current_live_japanese.as_str())
            .wrap(Wrap { trim: true })
            .block(live_japanese_block);
        frame.render_widget(live_japanese_paragraph, main_layout[1]);

        let history_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_layout[2]);

        let japanese_history_items: Vec<ListItem> = self
            .completed_japanese
            .iter()
            .rev() // Show newest first
            .map(|s| ListItem::new(s.as_str()))
            .collect();
        let japanese_history_list = List::new(japanese_history_items)
            .block(
                Block::default()
                    .title("Japanese Transcript")
                    .borders(Borders::ALL),
            )
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
            .highlight_symbol(">> ");
        frame.render_widget(japanese_history_list, history_layout[0]);

        let english_translation_items: Vec<ListItem> = self
            .completed_translations
            .iter()
            .rev() // Show newest first
            .map(|s| ListItem::new(s.as_str()))
            .collect();
        let english_translation_list = List::new(english_translation_items)
            .block(
                Block::default()
                    .title("English Translation")
                    .borders(Borders::ALL),
            )
            .style(Style::default().fg(Color::Cyan))
            .highlight_style(Style::default().add_modifier(Modifier::ITALIC))
            .highlight_symbol(">> ");
        frame.render_widget(english_translation_list, history_layout[1]);
    }
}

async fn audio_processing_task(tx: mpsc::Sender<AppUpdate>) -> Result<(), anyhow::Error> {
    tx.send(AppUpdate::StatusUpdate(
        "Initializing models...".to_string(),
    ))
    .await
    .ok();

    let whisper_model = WhisperBuilder::default()
        .with_language(Some(WhisperLanguage::Japanese)) // Specify Japanese
        .build()
        .await?;

    tx.send(AppUpdate::StatusUpdate(
        "Whisper model loaded. Initializing Llama...".to_string(),
    ))
    .await
    .ok();

    let llama_model = Llama::builder()
        .with_source(LlamaSource::qwen_2_5_7b_instruct()) // Or another suitable model
        .build()
        .await?;
    let mut llama_chat = llama_model.chat().with_system_prompt("You are an expert translator. Translate the given Japanese text to English accurately and concisely. Output only the English translation. Do not add any pleasantries or extra explanations.");

    tx.send(AppUpdate::StatusUpdate(
        "All models loaded. Listening for microphone input...".to_string(),
    ))
    .await
    .ok();

    let mic_input = MicInput::default();
    let mut audio_chunks = mic_input
        .stream()
        .voice_activity_stream()
        .rechunk_voice_activity();

    while let Some(input_audio_chunk) = audio_chunks.next().await {
        tx.send(AppUpdate::StatusUpdate("Transcribing audio...".to_string()))
            .await
            .ok();
        let mut current_segment_text = String::new();
        let mut transcribed_stream = whisper_model.transcribe(input_audio_chunk);

        while let Some(transcribed) = transcribed_stream.next().await {
            if transcribed.probability_of_no_speech() < 0.85 {
                // Adjust as needed
                // Adjust as needed
                current_segment_text.push_str(&transcribed.text());
                tx.send(AppUpdate::LiveJapaneseUpdate(current_segment_text.clone()))
                    .await
                    .ok();
            }
        }

        if current_segment_text.trim().chars().count() > 1 {
            tx.send(AppUpdate::JapaneseSegmentComplete(
                current_segment_text.clone(),
            ))
            .await
            .ok();
            tx.send(AppUpdate::StatusUpdate(
                "Translating to English...".to_string(),
            ))
            .await
            .ok();

            let prompt = current_segment_text; // The system prompt handles the instruction
            let mut response_stream = llama_chat(&prompt);
            let translation = response_stream.all_text().await; // Use all_text()

            if !translation.is_empty() {
                tx.send(AppUpdate::EnglishTranslation(translation))
                    .await
                    .ok();
            } else {
                // Handle cases where translation might be empty
                tx.send(AppUpdate::EnglishTranslation(
                    "[No translation generated]".to_string(),
                ))
                .await
                .ok();
            }
        } else {
            // Clear live japanese if segment was too short/empty
            tx.send(AppUpdate::LiveJapaneseUpdate("".to_string()))
                .await
                .ok();
        }
        tx.send(AppUpdate::StatusUpdate("Listening...".to_string()))
            .await
            .ok();
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let (tx, rx) = mpsc::channel(32); // Channel for AppUpdates

    // Clone tx for the audio processing task
    let tx_audio = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = audio_processing_task(tx_audio).await {
            // Send error to UI if task fails
            let _ = tx
                .send(AppUpdate::Error(format!(
                    "Audio processing task failed: {}",
                    e
                )))
                .await;
        }
    });

    // Setup terminal
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture // Though not used, good practice
    )?;
    terminal.clear()?; // Clear terminal before first draw

    let app = App::new(rx);
    let app_result = app.run(terminal);

    // Restore terminal
    crossterm::execute!(
        std::io::stdout(), // Use stdout directly for restore
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    crossterm::terminal::disable_raw_mode()?;

    if let Err(err) = app_result {
        println!("Error running app: {:?}", err);
        return Err(err.into());
    }

    Ok(())
}

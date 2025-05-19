# transvibe

Transcribe+Translate: Real-time, local transcription and translation.

> [!NOTE]
> This project is a "vibe coding" session with Gemini 2.5 Pro Preview, developed using the [Zed Editor](https://zed.dev).

<img width="712" alt="image" src="https://github.com/user-attachments/assets/ed3cb2f9-7f6e-4b8d-abf4-1a755d234543" />

## Features

-   🦀 **Built with Rust**: Crafted by Rustaceans using [Kalosm](https://floneum.com/kalosm/) for AI and [Ratatui](https://ratatui.rs) for the terminal interface.
-   🏡 **100% Offline**: Operates entirely locally, ensuring your data privacy and functionality without an internet connection.
-   🎤 **Real-time Transcription**: Captures and transcribes audio from your microphone as you speak.
-   ⚡ **Responsive Translation**: Utilizes a separate thread for translation, preventing delays in the transcription process.
-   ✨ **Enhanced Readability**: Highlights the most recent transcribed line, making it easier to follow along.

## TODO

Future enhancements, planned features, and currently missing capabilities include:

-   **Expanded Language Support**: Adding transcription and translation capabilities for languages beyond the current scope.
-   **File-based Transcription**: Implementing the ability to transcribe audio from various file formats (currently missing).
-   **Saving and Exporting**: Adding functionality to save and export transcriptions and translations (currently missing).
-   **User-configurable Settings**: Implementing options like audio input device selection (currently missing).
-   **Pause/Resume Functionality**: Adding the ability to pause and resume transcription (currently missing).
-   **Session Management**: Allowing users to save and load transcription/translation sessions.
-   **UI/UX Improvements**: Continuously refining the user interface and experience.
-   **Performance Optimizations**: Further optimizing processing for speed and resource efficiency.
-   **Customizable Models**: Allowing users to specify different speech-to-text or translation models.

## Run

To run the application:

```bash
cargo run --release
```

## Build

To build the application from source:

```bash
cargo build --release
```

# Camel

Translate comics with your own AI provider. Camel detects text, translates dialogue, removes the original lettering, and typesets the result. Review and edit each page in the browser, or use the CLI.

[![Camel workbench showing a translated comic page](assets/workbench.png)](assets/workbench.png)

The sample shows *Invincible* translated into Simplified Chinese. Artwork belongs to its respective rights holders. Click the screenshot to view it at full resolution.

## Features

- **Local image processing.** Analyze comic archives or image folders, detect text, and create masks and layouts in your browser or on the CLI.
- **Your AI provider.** Translate with an OpenAI-compatible API, using your own model, prompt, glossary, and target language. Translation can include page images or use text only.
- **Review and edit.** Correct OCR and dialogue, paint masks, adjust typesetting, and compare the original, erased, and translated pages.
- **Resume and export.** Save progress per page, check terminology and layout, and export a CBZ or PNG pages.

## Get started

1. Open the [workbench](https://sec-ant.github.io/camel/) in a desktop browser. A two-page example project opens on your first visit.
2. Choose a font that covers your target language.
3. Choose **New project**, select your comic and font, and download the default models or choose your own model folder.
4. Open **Settings** and enter your API URL, model, API key, and target language. The API must allow cross-origin browser requests.
5. Use **Jobs** to translate and export, and **Workbench** to review the result.

| File | Model |
| --- | --- |
| `detector.onnx` | [comic-text-and-bubble-detector, detector-v4-s_int8](https://huggingface.co/ogkalu/comic-text-and-bubble-detector) |
| `recognizer.onnx` | [RapidOCR, en_PP-OCRv5_rec_mobile](https://github.com/RapidAI/RapidOCR/blob/main/python/rapidocr/default_models.yaml), including its embedded character dictionary |

Chromium browsers can save directly to a project folder. Firefox and Safari use browser storage: select **Download changes** and unpack the ZIP over the project folder to save edits. Keep the tab open while jobs run.

## CLI

Install Rust and [just](https://github.com/casey/just), then:

```sh
just install
camel analyze comic.cbz -o book --models /path/to/models
CAMEL_API_KEY=your-key camel translate book --base-url https://example.com/v1 --model your-model
camel export book -o translated.cbz --font /path/to/font.ttf
```

Run `camel --help` or `camel <command> --help` for options, including review, terminology and layout checks. The CLI and workbench share the project format; original images, models, and fonts remain separate.

## Privacy

Image processing runs locally. Translation and model-assisted review send dialogue and, in visual mode, page images to your configured API. Browser settings and the API key stay in browser storage; the key is excluded from project files and exported configuration.

## Develop

Requirements: Rust, Node.js, [just](https://github.com/casey/just), `wasm-bindgen-cli` matching `Cargo.toml`, and Binaryen's `wasm-opt`.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
just web-dev  # serve at http://localhost:5173
just web      # build the workbench and run frontend tests
just check    # Rust formatting, Clippy, tests, and docs
```

The static build is written to `web/dist/`. GitHub Actions deploys it to GitHub Pages on pushes to `main`.

The starter project uses two pages from *Coffee for You* by [ArtAvenue](https://krita-artists.org/t/coffee-for-you-short-comic/3394), licensed under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). The pages have been translated into Chinese. The default preview font is [LXGW WenKai](https://github.com/lxgw/LxgwWenKai), licensed under the SIL Open Font License.

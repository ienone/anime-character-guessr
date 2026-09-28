[中文](README.md) | [English](README.en.md)

## 📖 Overview
Anime Character Guessr — have some fun guessing anime characters!

- A game where you guess anime characters. Best experienced on a desktop browser.
- Inspired by [BLAST.tv](https://blast.tv/counter-strike), data sourced from [Bangumi](https://bgm.tv/).
- Translation project by vertiKarl: [GitHub](https://github.com/vertiKarl/anime-character-guessr-english) / [Weblink](https://vertikarl.github.io/anime-character-guessr-english).

## 📦 Project Structure

- `client/`: Vite React frontend.
- `server-rs/`: Rust game server for Socket.IO, Bangumi API proxying, and image caching.
- `db-builder/`: Bangumi database build and maintenance tools.
- `archive.sqlite`: Local subject/character search index database.

## 🚀 Local Development

Frontend:
```bash
cd client
npm install
npm run dev
```

Rust server:
```bash
cd server-rs
cargo run
```

The frontend reads `VITE_SERVER_URL` for the server URL. If unset, it uses the same origin.

## 🐳 Docker

Create a root `.env` file:
```env
DOMAIN_NAME=http://[your IP]
SERVER_INTERNAL_PORT=3001
NGINX_EXTERNAL_PORT=80
```

Start the stack:
```bash
docker-compose up --build
```

Stop and remove containers:
```bash
docker-compose down
```

## 🚀 Self-Hosting Notes

- Bangumi API calls go through the server-side `/api/bgm/*` proxy.
- Small thumbnails use Bangumi `grid` images and are cached locally as `/img/{id}.webp`.
- Large previews return Bangumi source image URLs directly to avoid storing large images on the server.
- CDN/edge acceleration can cache both `/img/*` and source-image redirects to reduce origin pressure.

## 🎮 How to Play

- Guess a hidden anime character. Search for a character and make a guess.
- After each guess, you get information about the character you guessed.
- Green highlight: correct or very close; yellow highlight: somewhat close.
- `↑`: guess higher; `↓`: guess lower.

## ✨ Contributing Tags

- Keep asset and data paths organized when submitting external tag PRs.
- Place assets under `client/public/assets`.
- Put tag data under `client/public/data/extra_tags`; maintainers will review and import it.
- New tags not loading locally? Ensure subject IDs are added to `client/src/data/extra_tag_subjects.js`.

# Repository Guidelines

## Project Structure & Module Organization

This repository is split into a Vite React client, a Rust game server, and data/deploy tooling.

- `client/`: frontend app source, build config, and static assets.
- `client/src/`: React pages, components, utilities, data modules, and CSS.
- `client/public/assets/`: images, fonts, avatars, and tag icons.
- `client/public/data/extra_tags/`: contributed extra tag JSON.
- `server-rs/`: Rust Axum + socketioxide backend, archive/search/image-cache routes, and gameplay logic.
- `server-rs/src/`: Rust route handlers, socket state, database helpers, and game assembly code.
- `server-rs/data/tantivy/`: prebuilt Tantivy search index used by the Rust server in production-style runs.
- `db-builder/`: archive/database build utilities.
- `benchmarks/`: local stress and benchmark harnesses.
- `nginx/`, `deploy/`, `docker-compose.yml`, `.env.example`: deployment and container setup.

## Build, Test, and Development Commands

Install dependencies separately in `client/` and build the Rust server from `server-rs/`.

- `cd client && npm run dev`: start the Vite dev server.
- `cd client && npm run build`: create the production frontend build.
- `cd client && npm run lint`: run ESLint for JS/JSX.
- `cd client && npm run preview`: preview the built frontend.
- `cd server-rs && cargo run`: start the Rust backend locally.
- `cd server-rs && cargo check`: type-check the Rust backend.
- `cd server-rs && cargo test`: run Rust backend tests.
- `docker-compose up --build`: run client, Rust server, and nginx together.
- `cd benchmarks && npm run stress`: run the local stress harness when diagnosing runtime behavior.

## Coding Style & Naming Conventions

Use ES modules throughout JavaScript files. Client components and pages use PascalCase filenames such as `SearchBar.jsx`; utility modules use lower camel case or descriptive lowercase names. Keep UI logic in `client/src/components/` or `client/src/pages/`, shared data in `client/src/data/`, and frontend API helpers in `client/src/utils/`.

Follow the existing frontend style: two-space indentation, semicolon-free JavaScript, single quotes, and concise functional React components. Run `npm run lint` in `client/` before changing frontend code.

Rust code in `server-rs/` should be formatted with `cargo fmt`. Keep blocking SQLite/archive work inside the existing `db::with_*` helper APIs or explicit `spawn_blocking` paths instead of running it directly on async socket handlers.

## Testing Guidelines

Backend tests are Rust tests under `server-rs/` and run with `cargo test`. Add focused tests for gameplay rules, socket room state, archive/database behavior, and persistence logic when changing those surfaces. The client has linting but no unit test script, so validate UI changes with lint/build and manual browser checks for visible behavior.

## Commit & Pull Request Guidelines

Recent commits use short Conventional Commit-style prefixes, including `fix:`, `chore:`, and `Revert`, sometimes with Chinese descriptions. Keep messages concise and scoped, for example `fix: resolve multiplayer room cleanup`.

Pull requests should describe the change, list checks run, link related issues, and include screenshots for visible UI changes.

## Security & Configuration Tips

Copy `.env.example` to `.env` for Docker-based local runs. Do not commit real production domains, generated logs, local build output, or private admin tokens.

For the Rust server, production-style deployments need `ARCHIVE_DB_PATH`, `APP_DB_PATH`, `IMAGE_CACHE_DIR`, and `TANTIVY_INDEX_DIR`. Admin room maintenance routes such as `/clean-rooms` and `/close-room/{id}` require `ROOM_ADMIN_TOKEN`; pass it via `X-Admin-Token`, `Authorization: Bearer ...`, or the `token` query parameter.

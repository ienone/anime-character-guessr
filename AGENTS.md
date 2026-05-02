# Repository Guidelines

## Project Structure & Module Organization

This repository is split into a Vite React client and a Node.js game server.

- `client/`: frontend app source, build config, and static assets.
- `client/src/`: React pages, components, utilities, data modules, and CSS.
- `client/public/assets/`: images, fonts, avatars, and tag icons.
- `client/public/data/extra_tags/`: contributed extra tag JSON.
- `server/`: Express/Socket.IO backend and server-side game utilities.
- `server/tests/`: gameplay and multiplayer tests.
- `nginx/`, `docker-compose.yml`, `.env.example`: deployment and container setup.

## Build, Test, and Development Commands

Install dependencies separately in `client/` and `server/`.

- `cd client && npm run dev`: start the Vite dev server.
- `cd client && npm run build`: create the production frontend build.
- `cd client && npm run lint`: run ESLint for JS/JSX.
- `cd client && npm run preview`: preview the built frontend.
- `cd server && npm run dev`: start the backend with `nodemon`.
- `cd server && npm start`: start the backend with Node.
- `cd server && npm test`: run backend tests.
- `docker-compose up --build`: run client, server, MongoDB, and nginx together.

## Coding Style & Naming Conventions

Use ES modules throughout JavaScript files. Client components and pages use PascalCase filenames such as `SearchBar.jsx`; utility modules use lower camel case or descriptive lowercase names such as `cached-axios.js`. Keep UI logic in `client/src/components/` or `client/src/pages/`, shared data in `client/src/data/`, and server helpers in `server/utils/`.

Follow the existing style: two-space indentation, semicolon-free JavaScript, single quotes, and concise functional React components. Run `npm run lint` in `client/` before changing frontend code.

## Testing Guidelines

Backend tests live in `server/tests/` and use `*.test.js` filenames. Add focused tests for gameplay rules, socket flows, and persistence logic. Run `cd server && npm test` before submitting server changes. The client has linting but no test script, so validate UI changes manually.

## Commit & Pull Request Guidelines

Recent commits use short Conventional Commit-style prefixes, including `fix:`, `chore:`, and `Revert`, sometimes with Chinese descriptions. Keep messages concise and scoped, for example `fix: resolve multiplayer room cleanup`.

Pull requests should describe the change, list checks run, link related issues, and include screenshots for visible UI changes. For tag contributions, place assets under `client/public/assets/`, JSON under `client/public/data/extra_tags/`, and update `client/src/data/extra_tag_subjects.js` when needed.

## Security & Configuration Tips

Copy `.env.example` to `.env` for Docker-based local runs. Do not commit real `AES_SECRET`, MongoDB credentials, production domains, generated logs, or local build output.

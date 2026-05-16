MAKE := make

.PHONY: help install dev dev-client dev-server preview build build-client build-server check check-server check-db-builder lint lint-client clippy clippy-server clippy-db-builder test test-server fmt docker-up docker-down docker-logs

help:
	@echo "Available targets:"
	@echo "  make install          Install frontend dependencies"
	@echo "  make dev              Start frontend and Rust server concurrently"
	@echo "  make dev-client       Start Vite dev server"
	@echo "  make dev-server       Start Rust backend"
	@echo "  make preview          Preview built frontend"
	@echo "  make build            Build frontend and Rust backend"
	@echo "  make check            Run Rust cargo check and frontend lint"
	@echo "  make lint             Run frontend lint and Rust clippy"
	@echo "  make test             Run Rust backend tests"
	@echo "  make fmt              Format Rust crates"
	@echo "  make docker-up        Start Docker deployment stack"
	@echo "  make docker-down      Stop Docker deployment stack"
	@echo "  make docker-logs      Follow Docker logs"

install:
	cd client && npm install

dev:
	$(MAKE) -j2 dev-client dev-server

dev-client:
	cd client && npm run dev

dev-server:
	cd server-rs && cargo run

preview:
	cd client && npm run preview

build: build-client build-server

build-client:
	cd client && npm run build

build-server:
	cd server-rs && cargo build --release

check: check-server check-db-builder lint-client

check-server:
	cd server-rs && cargo check

check-db-builder:
	cd db-builder && cargo check

lint: lint-client clippy

lint-client:
	cd client && npm run lint

clippy: clippy-server clippy-db-builder

clippy-server:
	cd server-rs && cargo clippy -- -D warnings

clippy-db-builder:
	cd db-builder && cargo clippy -- -D warnings

test: test-server

test-server:
	cd server-rs && cargo test

fmt:
	cd server-rs && cargo fmt
	cd db-builder && cargo fmt

docker-up:
	docker compose up --build

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f

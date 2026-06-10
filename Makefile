BIN     := mCp
CARGO   := cargo
RELEASE := target/release/$(BIN)

# Capture any extra words after the target as the path argument
# Usage: make run /some/path
ifeq (run,$(firstword $(MAKECMDGOALS)))
  RUN_PATH := $(or $(wordlist 2,$(words $(MAKECMDGOALS)),$(MAKECMDGOALS)),/System/Volumes/Data)
  $(eval $(RUN_PATH):;@:)
endif

.PHONY: all build run clean

all: build

build:
	$(CARGO) build --release --bin $(BIN) 2>&1

run: build
	$(RELEASE) $(RUN_PATH)

clean:
	$(CARGO) clean

WASM_PACK ?= wasm-pack
CARGO_NDK ?= cargo ndk
ANDROID_API ?= 35
ANDROID_ABI ?= arm64-v8a
BIN ?= vive-converter

.PHONY: wasm android vive-converter

wasm:
	$(WASM_PACK) build --release --target web --out-dir pkg

android:
	$(CARGO_NDK) -t $(ANDROID_ABI) --platform $(ANDROID_API) build --release --bin $(BIN)

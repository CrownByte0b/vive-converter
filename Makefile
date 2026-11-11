WASM_PACK ?= wasm-pack
CARGO_NDK ?= cargo ndk
ANDROID_API ?= 35
ANDROID_ABI ?= arm64-v8a
BIN ?= vive-converter
NATIVE_TARGET ?=
# The library ViveNotes loads. Its Gradle build runs the same command into its own build directory.
ANDROID_LIB_ABIS ?= arm64-v8a x86_64
ANDROID_LIB_OUT ?= target/jniLibs

.PHONY: wasm android android-lib native-lib vive-converter

wasm:
	$(WASM_PACK) build --release --target web --out-dir pkg

android:
	$(CARGO_NDK) -t $(ANDROID_ABI) --platform $(ANDROID_API) build --release --bin $(BIN)

android-lib:
	$(CARGO_NDK) $(foreach abi,$(ANDROID_LIB_ABIS),-t $(abi)) --platform $(ANDROID_API) -o $(ANDROID_LIB_OUT) build --locked --profile android --lib
	# onenote_parser declares a cdylib of its own; the converter links it statically.
	rm -f $(ANDROID_LIB_OUT)/*/libonenote_parser.so

native-lib:
	cargo build --locked --profile native --lib $(if $(NATIVE_TARGET),--target $(NATIVE_TARGET))

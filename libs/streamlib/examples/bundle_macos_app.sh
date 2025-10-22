#!/bin/bash
# Bundle Rust binary as macOS app with Info.plist

set -e

# Configuration
BINARY_NAME="camera_display"
APP_NAME="CameraDisplay.app"
TARGET_DIR="target/debug/examples"

# Build the binary first
echo "Building $BINARY_NAME..."
cargo build --example $BINARY_NAME

# Create app bundle structure
echo "Creating app bundle..."
rm -rf "$TARGET_DIR/$APP_NAME"
mkdir -p "$TARGET_DIR/$APP_NAME/Contents/MacOS"
mkdir -p "$TARGET_DIR/$APP_NAME/Contents/Resources"

# Copy binary
cp "$TARGET_DIR/$BINARY_NAME" "$TARGET_DIR/$APP_NAME/Contents/MacOS/$BINARY_NAME"

# Copy Info.plist
cp "libs/streamlib/examples/Info.plist" "$TARGET_DIR/$APP_NAME/Contents/Info.plist"

echo "✅ App bundle created at: $TARGET_DIR/$APP_NAME"
echo "Run with: open $TARGET_DIR/$APP_NAME"

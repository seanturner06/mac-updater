#!/usr/bin/env bash
# install.sh — set up the update script and launchd schedule

set -euo pipefail

SCRIPT_DIR="$HOME/scripts"
PLIST_DIR="$HOME/Library/LaunchAgents"
LABEL="com.updateall"

echo "Setting up mac-updater..."

# Create scripts directory
mkdir -p "$SCRIPT_DIR"

# Copy update script
cp update-all.sh "$SCRIPT_DIR/update-all.sh"
chmod +x "$SCRIPT_DIR/update-all.sh"
echo "  Copied update-all.sh to $SCRIPT_DIR/"

# Install plist with paths updated for the current user
mkdir -p "$PLIST_DIR"
sed "s|/HOME_PLACEHOLDER|$HOME|g" "$LABEL.plist" > "$PLIST_DIR/$LABEL.plist"
echo "  Installed plist to $PLIST_DIR/"

# Unload first in case it's already loaded from a previous install
launchctl unload "$PLIST_DIR/$LABEL.plist" 2>/dev/null || true

# Load the schedule
launchctl load "$PLIST_DIR/$LABEL.plist"
echo "  Loaded launchd schedule (daily at 10:00 AM)"

echo ""
echo "Done! Test it with:"
echo "  ~/scripts/update-all.sh"
echo "  cat ~/scripts/update-all.log"

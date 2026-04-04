#!/bin/sh

SESSION_FILE=".mcp_session"

if [ ! -f "$SESSION_FILE" ]; then
    echo "No session file found. Run ./initialize.sh first."
    exit 1
fi

source "$SESSION_FILE"

if [ -z "$SERVER" ] || [ -z "$SESSION_ID" ] || [ -z "$NEXT_REQUEST_ID" ]; then
    echo "Invalid session file. Run ./initialize.sh to create a new session."
    exit 1
fi

CURRENT_REQUEST_ID=$NEXT_REQUEST_ID
NEXT_REQUEST_ID=$((NEXT_REQUEST_ID + 1))
echo "SERVER=$SERVER" > "$SESSION_FILE"
echo "PROTOCOL_VERSION=$PROTOCOL_VERSION" >> "$SESSION_FILE"
echo "SESSION_ID=$SESSION_ID" >> "$SESSION_FILE"
echo "NEXT_REQUEST_ID=$NEXT_REQUEST_ID" >> "$SESSION_FILE"

TMPFILE=$(mktemp)

HTTP_CODE=$(curl -s -o "$TMPFILE" -w "%{http_code}" -X POST "$SERVER" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "MCP-Session-Id: $SESSION_ID" \
  -H "MCP-Protocol-Version: $PROTOCOL_VERSION" \
  -d "{
    \"jsonrpc\": \"2.0\",
    \"id\": $CURRENT_REQUEST_ID,
    \"method\": \"tools/list\",
    \"params\": {}
  }")

if [ "$HTTP_CODE" = "401" ] || [ "$HTTP_CODE" = "404" ]; then
    echo "Error: Session expired or not found. Run ./initialize.sh to create a new session." >&2
    rm -f "$TMPFILE"
    exit 1
fi

if [ ! -s "$TMPFILE" ]; then
    echo "Error: No response from server." >&2
    rm -f "$TMPFILE"
    exit 1
fi

grep '^data: {' "$TMPFILE" | sed 's/^data: //' | jq .
rm -f "$TMPFILE"

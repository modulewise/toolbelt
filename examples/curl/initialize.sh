#!/bin/sh

SERVER="${1:-http://127.0.0.1:3001/mcp}"
SESSION_FILE=".mcp_session"

INIT_FULL_RESPONSE=$(curl -s -X POST "$SERVER" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -D - \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-06-18",
      "capabilities": {},
      "clientInfo": {"name": "curl-client", "version": "0.1.0"}
    }
  }')

SESSION_ID=$(echo "$INIT_FULL_RESPONSE" | grep -i "mcp-session-id" | cut -d: -f2 | tr -d ' \r\n')
INIT_RESPONSE=$(echo "$INIT_FULL_RESPONSE" | grep "^data:")

if [ -z "$SESSION_ID" ]; then
    echo "Error: Failed to initialize session with server $SERVER" >&2
    exit 1
fi

PROTOCOL_VERSION=$(echo "$INIT_RESPONSE" | sed 's/^data: //' | jq -r '.result.protocolVersion')
echo "$INIT_RESPONSE" | sed 's/^data: //' | jq .

# Send initialized notification to server
curl -s -X POST "$SERVER" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "MCP-Session-Id: $SESSION_ID" \
  -d '{
    "jsonrpc": "2.0",
    "method": "notifications/initialized"
  }' > /dev/null

# Save session state to file
echo "SERVER=$SERVER" > "$SESSION_FILE"
echo "PROTOCOL_VERSION=$PROTOCOL_VERSION" >> "$SESSION_FILE"
echo "SESSION_ID=$SESSION_ID" >> "$SESSION_FILE"
echo "NEXT_REQUEST_ID=2" >> "$SESSION_FILE"

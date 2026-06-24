#!/bin/bash
set -e
FILE=${1:-corpus/raw/openxenium-bom.csv}
echo "Testing parse with: $FILE"
curl -s -X POST http://localhost:3001/v1/parse \
  -F "file=@$FILE" | jq .

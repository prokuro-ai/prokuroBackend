#!/bin/bash
set -e
FILE=${1:-corpus/raw/openxenium-bom.csv}
echo "Testing analyze with: $FILE"
curl -s -X POST http://localhost:3000/v1/analyze \
  -F "file=@$FILE" | jq .

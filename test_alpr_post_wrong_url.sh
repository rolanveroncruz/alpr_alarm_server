curl -v \
  -X POST \
  http://192.168.0.100/api/alpr/events \
  -H "Content-Type: application/json" \
  -d '{"camid":"TEST","date":"2026-10-07T18:00:00+08:00","plate":"ABC1234","plate_image":"","image":""}'
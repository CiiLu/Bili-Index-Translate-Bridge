> 该项目 *几乎* 是 100% AI 生成的，因此如果你觉得这是 AI Slop，那你的直觉是正确的。

### Run from source

```bash
git clone https://github.com/CiiLu/Bili-Index-Translate-Bridge.git
cd Bili-Index-Translate-Bridge

cargo run --release -- --host 127.0.0.1 --port 8080 --api-key "..."
```


### Docker Compose

```yaml
services:
  bili-bridge:
    image: ghcr.io/ciilu/bili-index-translate-bridge:latest
    container_name: bili-index-translate-bridge
    restart: unless-stopped
    ports:
      - "8080:8080"
    environment:
      - HOST=0.0.0.0
      - PORT=8080
#      - API_KEY=...
#      - API_BASE=https://index-translate.bilibili.com/v1
```

### 参考项目
https://github.com/bilibili/Index-Translate
https://github.com/Yeqingky/Index-Translate-Docker

### LICENSE
[The MIT License](LICENSE)


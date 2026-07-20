# Last Planned Features — Öncelikli Yol Haritası

**En önemli en üstte.** Dalgalar (Wave) = öncelik bandı.

Kaynak etiketleri: `#N` = `planned_features.md` id'si · `m#N` = tamamlayıcı öneri listesi · `yeni` = ikisinde de olmayan.
Her madde: **Kod** (dosya/modül/mekanizma düzeyinde ne yapılacak) + **Neden** (değer/gerekçe) + *efor*.

---


### Wave 0 — Hemen (patch sürüm · düşük efor / yüksek değer)

1. **Canary token'lar + token new-IP alert (#20, m#4)** — *efor: düşük*
   - **Kod:** `store/tokens.rs::ApiToken`'a `canary: bool`; token create/edit API'sine flag. `auth.rs` token doğrulama yolunda canary token sunulunca `emit_event`/`audit` ile `canary_tripped` (webhook + audit). New-IP: `state.rs`'e in-memory `token_seen_ips: Mutex<HashMap<String, HashSet<IpAddr>>>`; `tunnel/ws.rs` bağlanma yolunda ilk kez görülen IP'de `token_new_ip` event'i.
   - **Neden:** Sızan `aperio.yaml`/dump'ı net ihlal sinyaline çevirir; token store + alerting/webhook altyapısı hazır, ek maliyet minimal.

2. **Admin surface IP allowlist (m#2)** — *efor: düşük*
   - **Kod:** Yeni `APERIO_ADMIN_ALLOWED_IPS` (CIDR listesi; `settings.rs::ServerConfig` alanı, `routing::parse_trusted_proxies` ile parse). `main.rs`'te `/aperio` dashboard + `/aperio/api/*` route'larına guard: `extract_client_ip` ile eşleşmeyen IP → 403. Proxy trafiği ve tünel bağlantısı etkilenmez.
   - **Neden:** Dashboard/admin API en hassas yüzey; bugün yalnızca kimlik doğrulaması var, ağ katmanı kısıtı yok.

> İnsan notu: Dashboard ekranı ip block edilebilir olsada login ekranına erişilebilir olmalıdır, bu sayede şifreli servisler çalışmaya devam eder.

3. **Route-shadowing / duplicate-bind lint (m#40)** — *efor: düşük*
   - **Kod:** `check_config.rs`'e `routes:` ve bind çakışma analizi: aynı hostname+path prefix birden çok kez, veya ilk-eşleşen-kazanır sırasında erişilemez kalan route. `static_routes::compile` mantığına paralel; `FAIL`/`warn` olarak raporla.
   - **Neden:** Sessiz yanlış-routing'i deploy öncesi yakalar; `--check-config` altyapısı zaten var.

4. **Hot-reload audit diff (m#10)** — *efor: düşük*
   - **Kod:** `state.rs::reload_from_file`'da eski/yeni effective config anahtar farkını hesapla; `config_reloaded` audit detayına `key: old→new` ekle, secret pattern'li anahtarları (`*auth*`, `*token*`, `*secret*`) `redact.rs` yardımcısıyla maskele.
   - **Neden:** "Neden davranış değişti?" sorusunu audit'ten yanıtlar; bugün sadece "reloaded" yazıyor.

5. **Audit verify komutu (yeni; #113'ün kalan parçası)** — *efor: düşük*
   - **Kod:** `store/audit.rs::verify_chain` zaten var; `main.rs`'e `--verify-audit` alt komutu (`--check-config` gibi) — tüm rotasyon generation'larını + aktif dosyayı doğrula, kırık satırı raporla, exit 0/1. Opsiyonel `GET /aperio/api/audit/verify`.
   - **Neden:** Hash zincirinin tamper-evidence vaadini kullanıcıya görünür/otomatize kılar; zincir %90 hazır, sadece verify yüzeyi eksik.

6. **Zamanlanmış DB yedekleri (#108)** — *efor: düşük*
   - **Kod:** `retention.rs`'e benzer background task (`main.rs` spawn); `APERIO_BACKUP_INTERVAL`/`APERIO_BACKUP_DIR`/`APERIO_BACKUP_KEEP`. SQLite `VACUUM INTO '<dir>/aperio-<ts>.db'`; eski snapshot'ları retention ile prune; audit `db_backup`.
   - **Neden:** Self-hoster veri güvenliği; mantıksal export/import var ama fiziksel snapshot yok.

7. **i18n key coverage CI check (m#45)** — *efor: düşük*
   - **Kod:** `aperio-dashboard`'a küçük node/ts script'i: `i18n/index.tsx`'teki İngilizce anahtar kümesini referans alıp her dil dosyasında eksik/fazla/dup key'i bulsun, non-zero exit. `package.json` script + CI adımı.
   - **Neden:** Bu geliştirme sürecinde bizzat dup/eksik key sorunları çıktı; sessiz İngilizce-fallback'i yakalar.

8. **Prod-hardening checklist + threat-model docs (m#46, m#50)** — *efor: düşük*
   - **Kod:** `docs/` altında iki yeni md — canlıya-almadan-önce checklist (TLS, token hijyeni, lockout, retention, `trust_proxy`…) ve trust-boundary/threat-model (visitor↔server↔client↔backend).
   - **Neden:** Güvenli-varsayılan benimseme; "client sunucuya güvenmez" felsefesini resmileştirir.

### Wave 1 — Güvenlik & güvenilirlik çekirdeği (mevcut altyapı üstüne)

9. **Retry policy (#49)** — *efor: düşük-orta*
   - **Kod:** `proxy.rs` failover bloğu bugün yalnızca client-vanish (`None` yanıt) durumunda re-dispatch ediyor; onu **buffered 5xx** yanıtlar için de genişlet — yanıt gövdesi ziyaretçiye gitmeden önce `method_retryable` + `failover_max_jumps` ile başka client'a yeniden dene. `APERIO_RETRY_ON_5XX` / per-service.
   - **Neden:** Geçici backend 5xx'lerini şeffaf toparlar; failover makinesi neredeyse aynı işi yapıyor, ucuz genişletme.

> İnsan Notu: Bu senaryo tam olark ne tarafta olacak? client mi server mi? sanki bu belirsizlik problem yaratabilir. Ayrıca retry policy'i sadece 500'lu hatalarda mı yoksa diğer hatalarda da aktif olmalı mı? çok fazla soru işareti yarattı bu başlık altında. iyi düşünülmeli ve parametrik olarak ayarlanabilir olmalı.

10. **Protocol fuzzing (m#42)** — *efor: düşük-orta*
    - **Kod:** `cargo-fuzz` target'ları: `protocol.rs::decode_binary_frame` + `TunnelMessage` JSON/zlib decode (`decompress_frame`). `fuzz/` crate; CI'da kısa koşu. Frame ID-prefix invariant'ını (`id.len() <= 255`) da assert et.
    - **Neden:** Tünel frame decode ana bozulma/saldırı yüzeyi; parser panikleri/UB'yi erken yakalar (bkz. daha önce tespit edilen ID-truncation).

11. **Passive outlier ejection (#65)** — *efor: orta*
    - **Kod:** `state.rs::ClientHandle`'a kısa-pencere 5xx/timeout sayacı; `routing.rs::select_client_pool` eligibility filtresine "geçici olarak ejected" kontrolü + periyodik re-admit zamanlayıcısı. Aktif `/health` probing'den bağımsız.
    - **Neden:** Backend `/health` yeşilken canlı trafikte patladığında rotasyondan çıkarır — gerçek-dünya dayanıklılığı.

13. **Per-route / path rate limiting (yeni)** — *efor: orta*
    - **Kod:** `aperio-server.yaml`'a `rate_limits: [{hostname, path, rps, burst}]` structured section (`config_file::structured`, `error_pages` gibi). `proxy.rs` dispatch öncesi host+path eşleşen kurala göre token-bucket; yeni `route_rate: Mutex<HashMap<key, RateLimitState>>` (GC ile). 429.
    - **Neden:** Pahalı endpoint'i (login, export, arama) korur; bugün rate-limit yalnızca per-IP ve per-token.

14. **Scoped programmatic API keys (m#3 + yeni)** — *efor: orta*
    - **Kod:** Yeni `admin_keys` store (org + rol + scope alanları); `auth.rs`'te session-cookie'ye alternatif `Authorization: Bearer <admin_key>` doğrulaması; effective-org/rol key'den türesin. Dashboard'da Users/Settings altında yönetim + tek-seferlik secret gösterimi.
    - **Neden:** Otomasyon (CI, Terraform #90, Slack #96) all-powerful master token'a muhtaç kalmasın; least-privilege programmatik kimlik.

15. **WAF-lite (#1)** — *efor: orta*
    - **Kod:** `aperio-server.yaml` (veya client Ping) `waf:` kuralları — path regex / method / header eşleşmesi + body-size; `proxy.rs` dispatch öncesi eşleşen "deny" kuralında 403/413. `routing`/`redact` yardımcılarını yeniden kullan.
    - **Neden:** Public'e açılan servisler için temel istek filtreleme; rate-limit + IP allowlist'i tamamlar.

16. **Token-to-device TOFU pinning (#16)** — *efor: orta*
    - **Kod:** Client ilk dial-out'ta keypair üretip config'e persist etsin, Ping'de public key ilan etsin (`protocol.rs`); server ilk görülen key'i token'a pin'lesin (`store/tokens.rs::pinned_key`), sonraki bağlantıda uyuşmazsa reddet. `APERIO_TOKEN_PINNING`.
    - **Neden:** CI log'una/dump'a sızan token başka makineden replay edilirse full PKI olmadan reddedilir.

> İnsan notu: token pinning güzel fikir, ayarlarda security kısmı olur, o kısımda require client token pinning ayarı olur, burada önemli detay şu sadece, eğer pinning açıksa sadece 1 client o token'a pinleneceği için o ayarın sadece tek bir client için geçerli olacağını manuel key pair taşıma işlemini kullanıcının sorumluluğu olduğunu söylememiz gerekli. bu ayar tüm uygulamanın tek token çok bağlantı prensibini etkiliyor. bunu bilmeyen bir kullanıcı bu ayarı açarsa ve bu tokenla farklı keypairle bağlanmaya çalışırsa, loglarda uygun bir şekilde reject almalı. token pinleme ile iletişimde şifreli hale gelebilir dimi? 

17. **Per-service response timeout override (m#14)** — *efor: düşük*
    - **Kod:** `aperio-config::ServiceEntry`/`FileConfig`'e `response_timeout`; Ping'e alan (`protocol.rs`); `ClientHandle` + `SelectedClient`'a taşı; `proxy.rs` global `gateway_response_timeout` yerine per-dispatch değeri kullansın.
    - **Neden:** Global timeout tek yavaş servisi zorluyor; yavaş rapor/upload endpoint'ine ayrı bütçe.

### Wave 2 — Büyük kayalar (temel · yüksek efor · ayrı milestone)


20. **Per-org quotas + aylık kullanım/faturalama (m#36 + yeni)** — *efor: orta*
    - **Kod:** `store/orgs.rs`'e quota alanları (max_clients/tokens/users, max_bytes_month); enforcement token/client create + `add_token_bytes` yollarında org toplamına bakar. Aylık kullanım: `stats` `by_org` period bucket'ları zaten var (#29 altyapısı) → `GET /aperio/api/orgs/{id}/usage` + billing webhook.
    - **Neden:** Multi-tenant guardrail'i tamamlar; per-token kotadan farklı (org geneli toplam + faturalama).

21. **Per-org OIDC overrides (m#37)** — *efor: orta*
    - **Kod:** `oidc.rs::OidcRuntime`'ı global'den org-map'e taşı; `store/orgs.rs`'e issuer/client/allowed_emails alanları; login akışı effective-org'a göre IdP seçsin.
    - **Neden:** Her tenant kendi kimlik sağlayıcısıyla giriş yapabilsin.

22. **Zero-downtime restart / fd handoff (m#39)** — *efor: yüksek*
    - **Kod:** systemd socket activation (`LISTEN_FDS`) veya `SO_REUSEPORT` ile listener fd devri; mevcut graceful drain (`ServerShutdown`) + yeni process eski bağlantıları devralana kadar. `main.rs` listener bind mantığı.
    - **Neden:** Restart'ta tünelleri düşürmeden güncelleme; ama tokio'da graceful handoff gerçekten zor.

23. **HA / multi-server (#105)** — *efor: çok yüksek*
    - **Kod:** Paylaşımlı state backend (tokens/routes/stats/sessions) — SQLite yerine Postgres/Redis opsiyonu veya consensus katmanı; client'ın sunucular arası failover'ı. En büyük mimari değişim.
    - **Neden:** Tek-sunucu SPOF'unu kaldırır; kurumsal ölçek.

### Wave 3 — Ürün derinliği & DX (ngrok-parity)


27. **Cache derinliği paketi (m#16, m#17, m#20, m#18)** — *efor: orta*
    - **Kod:** (m#16) `GET /aperio/api/cache/stats` + dashboard kartı — giriş sayısı/byte/route hit-ratio (`cache.rs`'te sayaçlar). (m#17) negative caching — 404/410'u kısa TTL cache'le. (m#20) cache key customization — `utm_*` gibi paramları key'den düş, `Accept-Encoding` varyantı. (m#18) surrogate-key purge — backend `Surrogate-Key` header'ı, purge API etiketle silsin.
    - **Neden:** Mevcut cache + purge API'sinin doğal, standart CDN devamı.

28. **Routing paketi (m#13, m#11, m#12)** — *efor: orta*
    - **Kod:** (m#13) catch-all default service — claim edilmemiş hostname trafiğini atanmış bir client'a ver (`routing.rs::select_client_pool` fallback). (m#11) per-hostname fallback URL — client offline'ken 504 yerine origin'e redirect/proxy. (m#12) header/cookie-based routing — request header/cookie eşleşmesine göre client seç.
    - **Neden:** Wildcard tenant + zarif degradasyon + canary-by-header senaryoları.

29. **Serve mode: SPA fallback + custom 404 (m#22)** — *efor: düşük*
    - **Kod:** `client/serve.rs`'te bilinmeyen path → `index.html` (SPA catch-all) opsiyonu ve custom 404 sayfası; bugün yalnızca dizin-index (`index.html`) çözümü var.
    - **Neden:** React/Vue router'lı SPA'ları tek `--serve` ile doğru sunar.

30. **Dashboard cilası (m#31–35, m#38)** — *efor: orta*
    - **Kod:** İki capture'ı yan yana diff (inspector compare), capture permalink (URL param), bulk token ops (çoklu revoke/TTL/org taşı), token presets ("CI preview"/"read-only"/"30-day"), saved traffic filters (URL'de isimli preset), structured-section editörleri (`headers:`/`routes:`/`expose:`/`error_pages:` UI'dan) — mevcut dashboard component/api katmanları üstüne.
    - **Neden:** Günlük operasyon verimliliği; mevcut inspector/token/settings sayfalarına ekler.

### Wave 4 — Kalite, gözlem & dağıtım (sürekli akış)

31. **Dashboard testleri: vitest + Playwright (m#41)** — *efor: orta*
    - **Kod:** vitest unit + Playwright e2e, `package.json` script + CI job. Şu an frontend testi **sıfır**.
    - **Neden:** 30+ React component'i test edilmiyor; regresyon riski yüksek — öne al.

32. **Benchmarks + perf gate (m#43) · Windows e2e (m#44)** — *efor: orta*
    - **Kod:** criterion benchmark'ları (proxy/cache/routing hot-path) + CI'da regression eşiği; k6 soak script'i. Windows CI koşusu (`development.md` "Unix-only" diyor).
    - **Neden:** Performans regresyonunu ve platform kırılmalarını erken yakalar.

> İnsan Notu: windows e2e yapmayalım, geliştirme ortamımım mac, ürün primary olarak linux ortamı odaklı, windowsta sorun olması durumunda feedback yolunu tercih edeceğiz, o gün geldiğinde düşünelim, diğer yazıkdıkların tutarlı

33. **Gözlem cilası (m#7, m#6, m#8, m#9)** — *efor: düşük-orta*
    - **Kod:** (m#7) server self-health card — process RSS/CPU, DB dosya boyutu, cache doluluk (dashboard + endpoint). (m#6) top visitor IPs — in-memory rolling window (persist yok, gizlilik uyumlu). (m#8) inspector capture sampling — yoğunlukta yüzde/route bazlı capture. (m#9) CSV export — traffic history + bandwidth raporları.
    - **Neden:** Operasyonel görünürlük; mevcut stats/inspector altyapısına küçük eklemeler.


35. **Docs derinleştirme (m#47, m#48, m#49)** — *efor: düşük*
    - **Kod:** architecture deep-dive (tunnel protokol/threading/state), upgrade guide + client/server uyumluluk matrisi, perf tuning guide (`connections`/`max_concurrent`/cache/compression trade-off'ları).
    - **Neden:** Contributor onboarding ve operatör güveni.

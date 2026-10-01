//! Client S3 des sauvegardes (#289) : `PUT` (simple ou en plusieurs parties), `HEAD`,
//! `GET`, `LIST` et `DELETE` sur un bucket, signés SigV4, sans SDK. Tout stockage
//! compatible convient (MinIO, Scaleway, AWS) ; MinIO veut l'adressage en chemin.
//!
//! ```text
//!  archive ──► PUT {prefix}penelope-<date>.tar.gz.enc   (en parties au-delà de 64 Mo)
//!          ──► PUT {prefix}penelope-<date>.manifest.json
//!          ──► HEAD : la taille vue par le serveur est celle du fichier
//!          ──► LIST {prefix} ─► rotation ─► DELETE des archives en trop
//! ```
//!
//! Chaque erreur HTTP nomme sa cause (403 : clé ou droits ; 404 : bucket absent) plutôt
//! qu'un code seul : c'est ce que le propriétaire lit dans le message de la nuit.

use super::sigv4::{self, Credentials};
use chrono::Utc;
use penelope_kernel::config::BackupS3;
use penelope_platform::SecretStore;
use std::path::Path;
use std::time::Duration;

/// Au-delà, l'envoi passe en plusieurs parties.
pub const MULTIPART_THRESHOLD: u64 = 64 * 1024 * 1024;
/// Taille d'une partie (S3 exige au moins 5 Mo, sauf pour la dernière).
pub const PART_SIZE: u64 = 32 * 1024 * 1024;
/// Suffixe des archives chiffrées.
pub const ARCHIVE_SUFFIX: &str = ".tar.gz.enc";
/// Suffixe du manifeste posé à côté de chaque archive.
pub const MANIFEST_SUFFIX: &str = ".manifest.json";

/// Un objet listé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub last_modified: String,
    pub etag: String,
}

/// Ce qu'un envoi vérifié rend.
#[derive(Debug, Clone)]
pub struct Uploaded {
    pub bytes: u64,
    pub etag: String,
    pub parts: u32,
}

#[derive(Clone)]
pub struct S3Client {
    endpoint: reqwest::Url,
    bucket: String,
    region: String,
    path_style: bool,
    creds: Credentials,
    http: reqwest::Client,
    multipart_threshold: u64,
    part_size: u64,
}

impl S3Client {
    /// Depuis `[backup.s3]`, les identifiants résolus par le magasin de secrets.
    pub fn from_config(cfg: &BackupS3, secrets: &dyn SecretStore) -> anyhow::Result<S3Client> {
        let access_key = secrets
            .expand(&cfg.access_key_id)
            .map_err(|e| anyhow::anyhow!("backup.s3.access_key_id : {e}"))?;
        let secret_key = secrets
            .expand(&cfg.secret_access_key)
            .map_err(|e| anyhow::anyhow!("backup.s3.secret_access_key : {e}"))?;
        if access_key.trim().is_empty() || secret_key.trim().is_empty() {
            anyhow::bail!("backup.s3 : clé d'accès vide");
        }
        S3Client::new(
            &cfg.endpoint,
            &cfg.bucket,
            &cfg.region,
            cfg.path_style,
            Credentials {
                access_key: access_key.trim().to_string(),
                secret_key: secret_key.trim().to_string(),
            },
        )
    }

    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        path_style: bool,
        creds: Credentials,
    ) -> anyhow::Result<S3Client> {
        let endpoint = reqwest::Url::parse(endpoint.trim())
            .map_err(|e| anyhow::anyhow!("backup.s3.endpoint `{endpoint}` : {e}"))?;
        if endpoint.host_str().is_none() {
            anyhow::bail!("backup.s3.endpoint `{endpoint}` : hôte manquant");
        }
        let http = reqwest::Client::builder()
            // Une archive de plusieurs centaines de Mo sur une ligne lente : une partie à
            // la fois, chacune bornée.
            .timeout(Duration::from_secs(900))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(S3Client {
            endpoint,
            bucket: bucket.trim().to_string(),
            region: region.trim().to_string(),
            path_style,
            creds,
            http,
            multipart_threshold: MULTIPART_THRESHOLD,
            part_size: PART_SIZE,
        })
    }

    /// Seuil et taille de partie plus petits (tests).
    pub fn with_part_sizes(mut self, threshold: u64, part: u64) -> Self {
        self.multipart_threshold = threshold;
        self.part_size = part.max(1);
        self
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// L'adresse, sans chemin ni identifiants, pour les messages.
    pub fn endpoint(&self) -> String {
        let mut u = self.endpoint.clone();
        u.set_path("");
        u.set_query(None);
        u.to_string().trim_end_matches('/').to_string()
    }

    /// Adresse et chemin signé d'une clé (`None` : le bucket).
    fn target(&self, key: Option<&str>) -> anyhow::Result<(reqwest::Url, String)> {
        let mut url = self.endpoint.clone();
        let base = self.endpoint.path().trim_end_matches('/').to_string();
        let path = if self.path_style {
            match key {
                Some(k) => format!("{base}/{}/{k}", self.bucket),
                None => format!("{base}/{}", self.bucket),
            }
        } else {
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("adresse S3 sans hôte"))?
                .to_string();
            url.set_host(Some(&format!("{}.{host}", self.bucket)))?;
            match key {
                Some(k) => format!("{base}/{k}"),
                None => format!("{base}/"),
            }
        };
        // Le chemin posé dans l'adresse est exactement celui qui est signé.
        url.set_path(&sigv4::uri_encode(&path, true));
        Ok((url, path))
    }

    /// Envoie une requête signée. Le corps tient en mémoire : une partie au plus.
    async fn send(
        &self,
        method: &str,
        key: Option<&str>,
        query: &[(String, String)],
        body: Vec<u8>,
    ) -> anyhow::Result<reqwest::Response> {
        let (mut url, path) = self.target(key)?;
        if !query.is_empty() {
            let mut pairs: Vec<(String, String)> = query
                .iter()
                .map(|(k, v)| (sigv4::uri_encode(k, false), sigv4::uri_encode(v, false)))
                .collect();
            pairs.sort();
            let qs = pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query(Some(&qs));
        }
        let mut host = url.host_str().unwrap_or_default().to_string();
        if let Some(p) = url.port() {
            host.push_str(&format!(":{p}"));
        }
        let hash = sigv4::payload_hash(&body);
        let headers = vec![
            ("host".to_string(), host),
            ("x-amz-content-sha256".to_string(), hash.clone()),
        ];
        let signed = sigv4::sign(
            &sigv4::Request {
                method,
                path: &path,
                query,
                headers: &headers,
                payload_hash: &hash,
                now: Utc::now(),
                region: &self.region,
                service: "s3",
            },
            &self.creds,
        );
        let mut rb = self
            .http
            .request(reqwest::Method::from_bytes(method.as_bytes())?, url)
            .header("x-amz-content-sha256", hash)
            .body(body);
        for (k, v) in signed {
            rb = rb.header(k, v);
        }
        rb.send()
            .await
            .map_err(|e| anyhow::anyhow!("{} injoignable : {e}", self.endpoint()))
    }

    /// Une réponse en erreur devient un message qui nomme la cause.
    async fn check(
        &self,
        resp: reqwest::Response,
        what: &str,
    ) -> anyhow::Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let code = xml_text(&body, "Code").unwrap_or_default();
        let message = xml_text(&body, "Message").unwrap_or_default();
        let bucket = &self.bucket;
        let cause = match (status.as_u16(), code.as_str()) {
            (403, _) => format!(
                "accès refusé (403 {code}) : clé d'accès inconnue, signature rejetée ou \
                 droits insuffisants sur le bucket `{bucket}`"
            ),
            (404, "NoSuchKey") => "objet introuvable (404 NoSuchKey)".to_string(),
            (404, _) => format!(
                "bucket `{bucket}` introuvable sur {} (404 {code})",
                self.endpoint()
            ),
            (301, _) | (400, "AuthorizationHeaderMalformed") => format!(
                "région ou adressage incorrect ({} {code}) : {message}",
                status.as_u16()
            ),
            _ => format!("HTTP {} {code} {message}", status.as_u16()),
        };
        anyhow::bail!("{what} : {}", cause.trim())
    }

    /// Le bucket répond : `HEAD` sur lui.
    pub async fn head_bucket(&self) -> anyhow::Result<()> {
        let r = self.send("HEAD", None, &[], Vec::new()).await?;
        self.check(r, &format!("bucket `{}`", self.bucket))
            .await
            .map(|_| ())
    }

    /// Envoie un objet tenu en mémoire ; renvoie son ETag.
    pub async fn put_bytes(&self, key: &str, body: Vec<u8>) -> anyhow::Result<String> {
        let r = self.send("PUT", Some(key), &[], body).await?;
        let r = self.check(r, &format!("PUT {key}")).await?;
        Ok(header(&r, "etag"))
    }

    /// Envoie un fichier, en plusieurs parties au-delà du seuil, puis vérifie par `HEAD`
    /// que le serveur en a la taille exacte.
    pub async fn put_file(&self, key: &str, path: &Path) -> anyhow::Result<Uploaded> {
        let size = std::fs::metadata(path)?.len();
        let (etag, parts) = if size > self.multipart_threshold {
            self.put_multipart(key, path).await?
        } else {
            (self.put_bytes(key, std::fs::read(path)?).await?, 1)
        };
        let Some((remote, _)) = self.head(key).await? else {
            anyhow::bail!("{key} : absent du bucket juste après l'envoi");
        };
        if remote != size {
            anyhow::bail!("{key} : {remote} octets sur le serveur, {size} envoyés");
        }
        Ok(Uploaded {
            bytes: size,
            etag,
            parts,
        })
    }

    async fn put_multipart(&self, key: &str, path: &Path) -> anyhow::Result<(String, u32)> {
        let q = [("uploads".to_string(), String::new())];
        let r = self.send("POST", Some(key), &q, Vec::new()).await?;
        let r = self
            .check(r, &format!("PUT {key} : ouverture de l'envoi en parties"))
            .await?;
        let upload_id = xml_text(&r.text().await?, "UploadId")
            .ok_or_else(|| anyhow::anyhow!("PUT {key} : pas d'UploadId dans la réponse"))?;
        let result = self.upload_parts(key, path, &upload_id).await;
        if result.is_err() {
            // Les parties déjà reçues ne restent pas facturées : abandon, au mieux.
            let q = [("uploadId".to_string(), upload_id.clone())];
            let _ = self.send("DELETE", Some(key), &q, Vec::new()).await;
        }
        result
    }

    async fn upload_parts(
        &self,
        key: &str,
        path: &Path,
        upload_id: &str,
    ) -> anyhow::Result<(String, u32)> {
        use tokio::io::AsyncReadExt;
        let mut file = tokio::fs::File::open(path).await?;
        let mut etags: Vec<(u32, String)> = Vec::new();
        let mut number: u32 = 1;
        loop {
            let mut part = Vec::with_capacity(self.part_size as usize);
            (&mut file)
                .take(self.part_size)
                .read_to_end(&mut part)
                .await?;
            if part.is_empty() {
                break;
            }
            let q = [
                ("partNumber".to_string(), number.to_string()),
                ("uploadId".to_string(), upload_id.to_string()),
            ];
            let r = self.send("PUT", Some(key), &q, part).await?;
            let r = self
                .check(r, &format!("PUT {key} : partie {number}"))
                .await?;
            etags.push((number, header(&r, "etag")));
            number += 1;
        }
        let body: String = std::iter::once("<CompleteMultipartUpload>".to_string())
            .chain(etags.iter().map(|(n, e)| {
                format!(
                    "<Part><PartNumber>{n}</PartNumber><ETag>{}</ETag></Part>",
                    xml_escape(e)
                )
            }))
            .chain(std::iter::once("</CompleteMultipartUpload>".to_string()))
            .collect();
        let q = [("uploadId".to_string(), upload_id.to_string())];
        let r = self.send("POST", Some(key), &q, body.into_bytes()).await?;
        let r = self
            .check(r, &format!("PUT {key} : fin de l'envoi en parties"))
            .await?;
        // S3 peut répondre 200 avec une erreur dans le corps : il faut le lire.
        let text = r.text().await?;
        if let Some(code) = xml_text(&text, "Code") {
            anyhow::bail!(
                "PUT {key} : fin de l'envoi en parties refusée ({code} {})",
                xml_text(&text, "Message").unwrap_or_default()
            );
        }
        Ok((
            xml_text(&text, "ETag").unwrap_or_default(),
            etags.len() as u32,
        ))
    }

    /// Taille et ETag d'un objet ; `None` s'il n'existe pas.
    pub async fn head(&self, key: &str) -> anyhow::Result<Option<(u64, String)>> {
        let r = self.send("HEAD", Some(key), &[], Vec::new()).await?;
        if r.status().as_u16() == 404 {
            return Ok(None);
        }
        let r = self.check(r, &format!("HEAD {key}")).await?;
        let size = header(&r, "content-length").parse().unwrap_or(0);
        Ok(Some((size, header(&r, "etag"))))
    }

    /// Les objets sous un préfixe, toutes pages lues, triés par clé.
    pub async fn list(&self, prefix: &str) -> anyhow::Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
                ("max-keys".to_string(), "1000".to_string()),
            ];
            if let Some(t) = &token {
                q.push(("continuation-token".to_string(), t.clone()));
            }
            let r = self.send("GET", None, &q, Vec::new()).await?;
            let r = self
                .check(r, &format!("LIST {}/{prefix}", self.bucket))
                .await?;
            let text = r.text().await?;
            for block in xml_blocks(&text, "Contents") {
                out.push(Object {
                    key: xml_text(block, "Key").unwrap_or_default(),
                    size: xml_text(block, "Size")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    last_modified: xml_text(block, "LastModified").unwrap_or_default(),
                    etag: xml_text(block, "ETag").unwrap_or_default(),
                });
            }
            token = match xml_text(&text, "IsTruncated").as_deref() {
                Some("true") => xml_text(&text, "NextContinuationToken"),
                _ => None,
            };
            if token.is_none() {
                break;
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        let r = self.send("DELETE", Some(key), &[], Vec::new()).await?;
        self.check(r, &format!("DELETE {key}")).await.map(|_| ())
    }

    /// Télécharge un objet dans un fichier ; renvoie les octets écrits.
    pub async fn get_to_file(&self, key: &str, dest: &Path) -> anyhow::Result<u64> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        let r = self.send("GET", Some(key), &[], Vec::new()).await?;
        let r = self.check(r, &format!("GET {key}")).await?;
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let mut file = tokio::fs::File::create(dest).await?;
        let mut written = 0u64;
        let mut stream = r.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow::anyhow!("GET {key} : {e}"))?;
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
        }
        file.flush().await?;
        Ok(written)
    }
}

fn header(r: &reqwest::Response, name: &str) -> String {
    r.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

// ------------------------------------------------------------------ archives

/// Préfixe normalisé : vide, ou terminé par `/`.
pub fn normalized_prefix(prefix: &str) -> String {
    let p = prefix.trim().trim_start_matches('/');
    if p.is_empty() || p.ends_with('/') {
        p.to_string()
    } else {
        format!("{p}/")
    }
}

/// Clé du manifeste d'une archive.
pub fn manifest_key(archive_key: &str) -> String {
    format!(
        "{}{MANIFEST_SUFFIX}",
        archive_key.trim_end_matches(ARCHIVE_SUFFIX)
    )
}

/// Les archives sous le préfixe, la plus récente d'abord (leur nom porte la date).
pub async fn list_archives(client: &S3Client, prefix: &str) -> anyhow::Result<Vec<Object>> {
    let mut archives: Vec<Object> = client
        .list(prefix)
        .await?
        .into_iter()
        .filter(|o| o.key.ends_with(ARCHIVE_SUFFIX))
        .collect();
    archives.sort_by(|a, b| b.key.cmp(&a.key));
    Ok(archives)
}

// ------------------------------------------------------------------ XML

/// Contenu du premier `<tag>…</tag>`, entités défaites.
pub(crate) fn xml_text(xml: &str, tag: &str) -> Option<String> {
    xml_blocks(xml, tag).first().map(|s| xml_unescape(s))
}

/// Contenus de tous les `<tag>…</tag>`, dans l'ordre.
pub(crate) fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

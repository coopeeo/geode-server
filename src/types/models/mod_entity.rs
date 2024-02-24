use crate::{
    endpoints::mods::{IndexQueryParams, IndexSortType},
    types::{
        api::{ApiError, PaginatedData},
        mod_json::ModJson,
        models::mod_version::ModVersion,
    },
};
use actix_web::web::Bytes;
use serde::Serialize;
use sqlx::{
    types::chrono::{DateTime, Utc},
    PgConnection, Postgres, QueryBuilder,
};
use std::{io::Cursor, str::FromStr};

use super::{
    developer::{Developer, FetchedDeveloper},
    mod_gd_version::{DetailedGDVersion, ModGDVersion, VerPlatform},
    tag::Tag,
};

#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Mod {
    pub id: String,
    pub repository: Option<String>,
    pub latest_version: String,
    pub validated: bool,
    pub featured: bool,
    pub download_count: i32,
    pub developers: Vec<Developer>,
    pub versions: Vec<ModVersion>,
    pub tags: Vec<String>,
    pub about: Option<String>,
    pub changelog: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ModRecord {
    id: String,
    #[sqlx(default)]
    repository: Option<String>,
    latest_version: String,
    validated: bool,
    download_count: i32,
    featured: bool,
    _updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ModRecordGetOne {
    id: String,
    repository: Option<String>,
    latest_version: String,
    validated: bool,
    featured: bool,
    version_id: i32,
    mod_download_count: i32,
    name: String,
    description: Option<String>,
    version: String,
    download_link: String,
    mod_version_download_count: i32,
    hash: String,
    geode: String,
    early_load: bool,
    api: bool,
    mod_id: String,
    about: Option<String>,
    changelog: Option<String>,
}

impl Mod {
    pub async fn get_index(
        pool: &mut PgConnection,
        query: IndexQueryParams,
    ) -> Result<PaginatedData<Mod>, ApiError> {
        let tags = match query.tags {
            Some(t) => Tag::parse_tags(&t, pool).await?,
            None => vec![],
        };
        let page = query.page.unwrap_or(1);
        if page <= 0 {
            return Err(ApiError::BadRequest(
                "Invalid page number, must be >= 1".into(),
            ));
        }
        let per_page = query.per_page.unwrap_or(10);
        let limit = per_page;
        let offset = (page - 1) * per_page;
        let mut platforms: Vec<VerPlatform> = vec![];
        if query.platforms.is_some() {
            for i in query.platforms.unwrap().split(',') {
                let trimmed = i.trim();
                let platform = VerPlatform::from_str(trimmed).or(Err(ApiError::BadRequest(
                    format!("Invalid platform {}", trimmed),
                )))?;
                if platform == VerPlatform::Android {
                    platforms.push(VerPlatform::Android32);
                    platforms.push(VerPlatform::Android64);
                } else {
                    platforms.push(platform)
                }
            }
        }
        let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT * FROM (SELECT DISTINCT m.id, m.repository, m.latest_version, mv.validated, m.about, m.changelog, m.download_count, m.featured, m.updated_at as _updated_at FROM mods m
            INNER JOIN mod_versions mv ON m.id = mv.mod_id
            INNER JOIN mod_gd_versions mgv ON mgv.mod_id = mv.id "
        );
        let mut counter_builder: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT COUNT(DISTINCT m.id) FROM mods m
            INNER JOIN mod_versions mv ON m.id = mv.mod_id
            INNER JOIN mod_gd_versions mgv ON mgv.mod_id = mv.id ",
        );

        if !tags.is_empty() {
            let sql = "INNER JOIN mods_mod_tags mmt ON mmt.mod_id = m.id ";
            builder.push(sql);
            counter_builder.push(sql);
        }

        if query.developer.is_some() {
            let sql = "INNER JOIN mods_developers md ON md.mod_id = m.id ";
            builder.push(sql);
            counter_builder.push(sql);
        }

        builder.push("WHERE ");
        counter_builder.push("WHERE ");

        if !tags.is_empty() {
            let sql = "mmt.tag_id = ANY(";
            builder.push(sql);
            counter_builder.push(sql);

            builder.push_bind(&tags);
            counter_builder.push_bind(&tags);
            let sql = ") AND ";
            builder.push(sql);
            counter_builder.push(sql);
        }

        if let Some(f) = query.featured {
            let sql = "m.featured = ";
            builder.push(sql);
            counter_builder.push(sql);
            builder.push_bind(f);
            counter_builder.push_bind(f);
            let sql = " AND ";
            builder.push(sql);
            counter_builder.push(sql);
        }

        let developer = match query.developer {
            Some(d) => match Developer::find_by_username(&d, pool).await? {
                Some(d) => Some(d),
                None => {
                    return Ok(PaginatedData {
                        data: vec![],
                        count: 0,
                    })
                }
            },
            None => None,
        };

        if let Some(d) = developer {
            let sql = "md.developer_id = ";
            builder.push(sql);
            counter_builder.push(sql);
            builder.push_bind(d.id);
            counter_builder.push_bind(d.id);
            let sql = " AND ";
            builder.push(sql);
            counter_builder.push(sql);
        }

        let sql = "mv.validated = true AND LOWER(mv.name) LIKE ";
        builder.push(sql);
        counter_builder.push(sql);

        let query_string = format!("%{}%", query.query.unwrap_or("".to_string()).to_lowercase());
        counter_builder.push_bind(&query_string);
        builder.push_bind(&query_string);
        if let Some(g) = query.gd {
            let sql = " AND mgv.gd = ";
            builder.push(sql);
            builder.push_bind(g);
            counter_builder.push(sql);
            counter_builder.push_bind(g);
        }
        for (i, platform) in platforms.iter().enumerate() {
            if i == 0 {
                let sql = " AND mgv.platform IN (";
                builder.push(sql);
                counter_builder.push(sql);
            }
            builder.push_bind(*platform);
            counter_builder.push_bind(*platform);
            if i == platforms.len() - 1 {
                builder.push(")");
                counter_builder.push(")");
            } else {
                builder.push(", ");
                counter_builder.push(", ");
            }
        }

        match query.sort {
            IndexSortType::Downloads => {
                builder.push(" ORDER BY m.download_count DESC");
            }
            IndexSortType::RecentlyUpdated => {
                builder.push(" ORDER BY m.updated_at DESC");
            }
            IndexSortType::RecentlyPublished => {
                builder.push(" ORDER BY m.created_at DESC");
            }
        }

        builder.push(") q LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let result = builder
            .build_query_as::<ModRecord>()
            .fetch_all(&mut *pool)
            .await;
        let records = match result {
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
            Ok(r) => r,
        };

        let result = counter_builder
            .build_query_scalar()
            .fetch_one(&mut *pool)
            .await;
        let count = match result {
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
            Ok(c) => c,
        };

        if records.is_empty() {
            return Ok(PaginatedData {
                data: vec![],
                count: 0,
            });
        }

        let ids: Vec<_> = records.iter().map(|x| x.id.clone()).collect();
        let versions = ModVersion::get_latest_for_mods(pool, &ids, query.gd, platforms).await?;
        let developers = Developer::fetch_for_mods(&ids, pool).await?;
        let mut mod_version_ids: Vec<i32> = vec![];
        for (_, mod_version) in versions.iter() {
            mod_version_ids.push(mod_version.id);
        }

        let gd_versions = ModGDVersion::get_for_mod_versions(&mod_version_ids, pool).await?;
        let tags = Tag::get_tags_for_mods(&ids, pool).await?;

        let ret = records
            .into_iter()
            .map(|x| {
                let mut version = versions.get(&x.id).cloned().unwrap();
                let gd_ver = gd_versions.get(&version.id).cloned().unwrap_or_default();
                version.gd = gd_ver;

                let devs = developers.get(&x.id).cloned().unwrap_or_default();
                let tags = tags.get(&x.id).cloned().unwrap_or_default();
                Mod {
                    id: x.id.clone(),
                    repository: x.repository.clone(),
                    latest_version: x.latest_version.clone(),
                    validated: x.validated,
                    download_count: x.download_count,
                    featured: x.featured,
                    versions: vec![version],
                    tags,
                    developers: devs,
                    about: None,
                    changelog: None,
                }
            })
            .collect();
        Ok(PaginatedData { data: ret, count })
    }

    pub async fn get_one(id: &str, pool: &mut PgConnection) -> Result<Option<Mod>, ApiError> {
        let records: Vec<ModRecordGetOne> = sqlx::query_as!(
            ModRecordGetOne,
            "SELECT
                m.id, m.repository, m.latest_version, mv.validated, m.about, m.changelog, m.featured, m.download_count as mod_download_count,
                mv.id as version_id, mv.name, mv.description, mv.version, mv.download_link, mv.download_count as mod_version_download_count,
                mv.hash, mv.geode, mv.early_load, mv.api, mv.mod_id
            FROM mods m
            INNER JOIN mod_versions mv ON m.id = mv.mod_id
            WHERE m.id = $1 AND mv.validated = true",
            id
        )
        .fetch_all(&mut *pool)
        .await
        .or(Err(ApiError::DbError))?;
        if records.is_empty() {
            return Ok(None);
        }
        let mut versions: Vec<ModVersion> = records
            .iter()
            .map(|x| ModVersion {
                id: x.version_id,
                name: x.name.clone(),
                description: x.description.clone(),
                version: x.version.clone(),
                download_link: x.download_link.clone(),
                download_count: x.mod_version_download_count,
                hash: x.hash.clone(),
                geode: x.geode.clone(),
                early_load: x.early_load,
                api: x.api,
                mod_id: x.mod_id.clone(),
                gd: DetailedGDVersion {
                    win: None,
                    android: None,
                    mac: None,
                    ios: None,
                    android32: None,
                    android64: None,
                },
                dependencies: None,
                incompatibilities: None,
            })
            .collect();
        let ids = versions.iter().map(|x| x.id).collect();
        let gd = ModGDVersion::get_for_mod_versions(&ids, pool).await?;
        let tags = Tag::get_tags_for_mod(id, pool).await?;
        let devs = Developer::fetch_for_mod(id, pool).await?;

        for i in &mut versions {
            let gd_versions = gd.get(&i.id).cloned().unwrap_or_default();
            i.gd = gd_versions;
        }

        let mod_entity = Mod {
            id: records[0].id.clone(),
            repository: records[0].repository.clone(),
            latest_version: records[0].latest_version.clone(),
            validated: records[0].validated,
            featured: records[0].featured,
            download_count: records[0].mod_download_count,
            versions,
            tags,
            developers: devs,
            about: records[0].about.clone(),
            changelog: records[0].changelog.clone(),
        };
        Ok(Some(mod_entity))
    }

    pub async fn from_json(
        json: &ModJson,
        developer: FetchedDeveloper,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        if semver::Version::parse(json.version.trim_start_matches('v')).is_err() {
            return Err(ApiError::BadRequest(format!(
                "Invalid mod version semver {}",
                json.version
            )));
        };

        if semver::Version::parse(json.geode.trim_start_matches('v')).is_err() {
            return Err(ApiError::BadRequest(format!(
                "Invalid geode version semver {}",
                json.geode
            )));
        };
        let dev_verified = developer.verified;

        Mod::create(json, developer, pool).await?;
        ModVersion::create_from_json(json, dev_verified, pool).await?;
        Ok(())
    }

    pub async fn new_version(
        json: &ModJson,
        developer: FetchedDeveloper,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let result = sqlx::query!(
            "SELECT DISTINCT m.id FROM mods m
            INNER JOIN mod_versions mv ON mv.mod_id = m.id
            WHERE m.id = $1 AND mv.validated = true",
            json.id
        )
        .fetch_optional(&mut *pool)
        .await
        .or(Err(ApiError::DbError))?;
        if result.is_none() {
            return Err(ApiError::NotFound(format!(
                "Mod {} doesn't exist or isn't yet validated",
                &json.id
            )));
        }

        let latest = sqlx::query!(
            "SELECT mv.version, mv.id FROM mod_versions mv
            INNER JOIN mods m ON mv.mod_id = m.id
            WHERE m.id = $1
            ORDER BY mv.id DESC LIMIT 1",
            &json.id
        )
        .fetch_one(&mut *pool)
        .await
        .unwrap();

        let version = semver::Version::parse(latest.version.trim_start_matches('v')).unwrap();
        let new_version = match semver::Version::parse(json.version.trim_start_matches('v')) {
            Ok(v) => v,
            Err(_) => {
                return Err(ApiError::BadRequest(format!(
                    "Invalid semver {}",
                    json.version
                )))
            }
        };
        if new_version.le(&version) {
            return Err(ApiError::BadRequest(format!(
                "mod.json version {} is smaller / equal to latest mod version {}",
                json.version, latest.version
            )));
        }
        ModVersion::create_from_json(json, developer.verified, pool).await?;

        if let Err(e) = sqlx::query!(
            "UPDATE mods SET image = $1, updated_at = now() WHERE id = $2",
            &json.logo,
            &json.id
        )
        .execute(&mut *pool)
        .await
        {
            log::error!("{}", e);
            return Err(ApiError::DbError);
        }

        Ok(())
    }

    /**
     * At the moment this only sets the mod to featured, can be expanded with more stuff
     */
    pub async fn update_mod(
        id: &str,
        featured: bool,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        if (match sqlx::query!("SELECT id FROM mods WHERE id = $1", id)
            .fetch_optional(&mut *pool)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
        })
        .is_none()
        {
            return Err(ApiError::NotFound(format!("Mod {} doesn't exist", id)));
        }

        let result = match sqlx::query!("UPDATE mods SET featured = $1 WHERE id = $2", featured, id)
            .execute(&mut *pool)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
        };

        if result.rows_affected() == 0 {
            return Err(ApiError::InternalError);
        }

        Ok(())
    }

    pub async fn get_logo_for_mod(
        id: &str,
        pool: &mut PgConnection,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        match sqlx::query!(
            "SELECT DISTINCT m.image FROM mods m
        INNER JOIN mod_versions mv ON mv.mod_id = m.id WHERE m.id = $1 AND mv.validated = true",
            id
        )
        .fetch_optional(&mut *pool)
        .await
        {
            Err(e) => {
                log::error!("{}", e);
                Err(ApiError::DbError)
            }
            Ok(r) => match r {
                Some(r) => Ok(r.image),
                None => Ok(None),
            },
        }
    }

    pub async fn try_update_latest_version(
        id: &str,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let latest = sqlx::query!(
            "SELECT mv.version, mv.id FROM mod_versions mv
            INNER JOIN mods m ON mv.mod_id = m.id
            WHERE m.id = $1 AND mv.validated = true
            ORDER BY mv.id DESC LIMIT 1",
            id
        )
        .fetch_optional(&mut *pool)
        .await;

        let latest = match latest {
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
            Ok(l) => l,
        };

        if latest.is_none() {
            return Ok(());
        }

        let latest = latest.unwrap();

        let result = sqlx::query!(
            "UPDATE mods SET latest_version = $1 WHERE id = $2",
            latest.version,
            id
        )
        .execute(&mut *pool)
        .await;

        match result {
            Err(e) => {
                log::error!("{}", e);
                Err(ApiError::DbError)
            }
            Ok(r) => {
                if r.rows_affected() == 0 {
                    log::info!("Something really bad happened with mod {}", id);
                    return Err(ApiError::InternalError);
                }

                Ok(())
            }
        }
    }

    async fn create(
        json: &ModJson,
        developer: FetchedDeveloper,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let res = sqlx::query!("SELECT id FROM mods WHERE id = $1", json.id)
            .fetch_optional(&mut *pool)
            .await
            .or(Err(ApiError::DbError))?;
        if res.is_some() {
            return Err(ApiError::BadRequest(format!(
                "Mod {} already exists, consider creating a new version",
                json.id
            )));
        }
        let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new("INSERT INTO mods (");
        if json.repository.is_some() {
            query_builder.push("repository, ");
        }
        if json.changelog.is_some() {
            query_builder.push("changelog, ");
        }
        if json.about.is_some() {
            query_builder.push("about, ");
        }
        query_builder.push("id, latest_version, image) VALUES (");
        let mut separated = query_builder.separated(", ");
        if json.repository.is_some() {
            separated.push_bind(json.repository.as_ref().unwrap());
        }
        if json.changelog.is_some() {
            separated.push_bind(&json.changelog);
        }
        if json.about.is_some() {
            separated.push_bind(&json.about);
        }
        separated.push_bind(&json.id);
        separated.push_bind(&json.version);
        separated.push_bind(&json.logo);
        separated.push_unseparated(")");

        let _ = query_builder
            .build()
            .execute(&mut *pool)
            .await
            .or(Err(ApiError::DbError))?;

        Mod::assign_owner(&json.id, developer.id, pool).await?;
        Ok(())
    }

    pub async fn calculate_cached_downloads(
        mod_id: &str,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        if let Err(e) = sqlx::query!(
            "UPDATE mods m SET download_count = m.download_count + (
                SELECT COUNT(DISTINCT md.ip) FROM mod_downloads md
                INNER JOIN mod_versions mv ON md.mod_version_id = mv.id
                WHERE mv.mod_id = m.id AND md.time_downloaded > m.last_download_cache_refresh AND mv.validated = true
            ), last_download_cache_refresh = now()
            WHERE m.id = $1", mod_id
        ).execute(&mut *pool).await {
            log::error!("{}", e);
            return Err(ApiError::DbError);
        }
        Ok(())
    }

    pub async fn assign_owner(
        mod_id: &str,
        dev_id: i32,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let existing = sqlx::query!(
            "SELECT md.developer_id, md.is_owner FROM mods_developers md
            INNER JOIN mods m ON md.mod_id = m.id
            WHERE m.id = $1",
            mod_id
        )
        .fetch_all(&mut *pool)
        .await;

        let existing = match existing {
            Err(e) => {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
            Ok(e) => e,
        };

        if !existing.is_empty() {
            let res = sqlx::query!(
                "UPDATE mods_developers SET is_owner = false
                WHERE mod_id = $1",
                mod_id
            )
            .execute(&mut *pool)
            .await;

            if let Err(e) = res {
                log::error!("{}", e);
                return Err(ApiError::DbError);
            }
        }

        for record in existing {
            // we found our dev inside the existing list
            if record.developer_id == dev_id {
                if let Err(e) = sqlx::query!(
                    "UPDATE mods_developers SET is_owner = true
                    WHERE mod_id = $1 AND developer_id = $2",
                    mod_id,
                    dev_id
                )
                .execute(&mut *pool)
                .await
                {
                    log::error!("{}", e);
                    return Err(ApiError::DbError);
                }
                return Ok(());
            }
        }

        if let Err(e) = sqlx::query!(
            "INSERT INTO mods_developers (mod_id, developer_id, is_owner) VALUES
            ($1, $2, true)",
            mod_id,
            dev_id
        )
        .execute(&mut *pool)
        .await
        {
            log::error!("{}", e);
            return Err(ApiError::DbError);
        }
        Ok(())
    }

    pub async fn assign_dev(
        mod_id: &str,
        dev_id: i32,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let existing = match sqlx::query!(
            "SELECT md.developer_id, md.is_owner FROM mods_developers md
            INNER JOIN mods m ON md.mod_id = m.id
            WHERE m.id = $1",
            mod_id
        )
        .fetch_all(&mut *pool)
        .await
        {
            Ok(e) => e,
            Err(err) => {
                log::error!("{}", err);
                return Err(ApiError::DbError);
            }
        };

        if existing.iter().any(|x| x.developer_id == dev_id) {
            return Err(ApiError::BadRequest(format!(
                "This developer already exists on mod {}",
                mod_id
            )));
        }

        match sqlx::query!(
            "INSERT INTO mods_developers (mod_id, developer_id)
            VALUES ($1, $2)",
            mod_id,
            dev_id
        )
        .execute(&mut *pool)
        .await
        {
            Err(err) => {
                log::error!("{}", err);
                Err(ApiError::DbError)
            }
            Ok(_) => Ok(()),
        }
    }

    pub async fn unassign_dev(
        mod_id: &str,
        dev_id: i32,
        pool: &mut PgConnection,
    ) -> Result<(), ApiError> {
        let existing = match sqlx::query!(
            "SELECT md.developer_id, md.is_owner FROM mods_developers md
            INNER JOIN mods m ON md.mod_id = m.id
            WHERE m.id = $1",
            mod_id
        )
        .fetch_all(&mut *pool)
        .await
        {
            Ok(e) => e,
            Err(err) => {
                log::error!("{}", err);
                return Err(ApiError::DbError);
            }
        };

        let found = match existing.iter().find(|x| x.developer_id == dev_id) {
            None => {
                return Err(ApiError::NotFound(
                    "Developer is not assigned to mod".to_string(),
                ))
            }
            Some(f) => f,
        };

        if found.is_owner {
            return Err(ApiError::BadRequest(
                "Cannot unassign the owner developer for the mod".to_string(),
            ));
        }

        match sqlx::query!(
            "DELETE FROM mods_developers
            WHERE mod_id = $1 AND developer_id = $2",
            mod_id,
            dev_id
        )
        .execute(&mut *pool)
        .await
        {
            Err(e) => {
                log::error!("{}", e);
                Err(ApiError::DbError)
            }
            Ok(_) => Ok(()),
        }
    }
}

pub async fn download_geode_file(url: &str) -> Result<Cursor<Bytes>, ApiError> {
    let res = reqwest::get(url)
        .await
        .or(Err(ApiError::BadRequest(String::from("Invalid URL"))))?;
    let content = Cursor::new(res.bytes().await.or(Err(ApiError::FilesystemError))?);
    Ok(content)
}

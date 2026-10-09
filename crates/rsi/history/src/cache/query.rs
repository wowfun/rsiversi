use super::*;
use rsi_history_api::{Match, QueryCursor, QueryProgress, QueryScope, SourceCoverage};
use std::collections::BTreeMap;

impl Cache {
    pub(crate) async fn remember(&self, scope: Scope, stop: CancellationToken) -> Result<()> {
        self.run(stop, move |db| {
            let source_key = key(&scope);
            if db
                .connection
                .query_row("SELECT 1 FROM catalog WHERE key=?1", [&source_key], |_| {
                    Ok(())
                })
                .optional()
                .map_err(sql)?
                .is_some()
            {
                return Ok(());
            }
            let encoded = serde_json::to_string(&scope).map_err(invalid)?;
            let (count, bytes): (i64, i64) = db
                .connection
                .query_row(
                    "SELECT count(*),coalesce(sum(length(CAST(scope AS BLOB))),0) FROM catalog",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(sql)?;
            if count >= 4096
                || bytes < 0
                || usize::try_from(bytes).map_err(invalid)? + encoded.len() > 1024 * 1024
            {
                return Err(ApiError::Capacity);
            }
            let tx = db.connection.transaction().map_err(sql)?;
            tx.execute(
                "INSERT INTO catalog VALUES(?1,?2)",
                params![source_key, encoded],
            )
            .map_err(sql)?;
            if tx
                .execute(
                    "UPDATE meta SET revision=revision+1 WHERE revision<9223372036854775807",
                    [],
                )
                .map_err(sql)?
                != 1
            {
                return Err(ApiError::Capacity);
            }
            tx.commit().map_err(sql)?;
            Ok(())
        })
        .await
    }
    pub(crate) async fn known(
        &self,
        range: QueryScope,
        stop: CancellationToken,
    ) -> Result<Vec<Scope>> {
        self.run(stop,move|db|{
            let mut retained=0usize;let mut count=0usize;
            let mut scopes=Vec::new();
            let mut statement=db.connection.prepare("SELECT length(CAST(scope AS BLOB)),CASE WHEN length(CAST(scope AS BLOB))<=8192 THEN scope END FROM catalog ORDER BY key LIMIT 4097").map_err(sql)?;
            let mut rows=statement.query([]).map_err(sql)?;
            while let Some(row)=rows.next().map_err(sql)? {
                let length=usize::try_from(row.get::<_,i64>(0).map_err(sql)?).map_err(invalid)?;
                retained=retained.checked_add(length).ok_or(ApiError::Capacity)?;count+=1;
                if length==0 || length>8192 || retained>2*1024*1024 || count>4096 {return Err(ApiError::Capacity);}
                let encoded:String=row.get(1).map_err(sql)?;
                let scope:Scope=serde_json::from_str(&encoded).map_err(invalid)?;
                if range.contains(&scope){scopes.push(scope);}
            }
            Ok(scopes)
        }).await
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The cache boundary receives query/cursor separately from the product-admitted principal, sources and progress."
    )]
    pub(crate) async fn query(
        &self,
        range: QueryScope,
        query: String,
        after: Option<QueryCursor>,
        caller: String,
        sources: Vec<SourceCoverage>,
        progress: QueryProgress,
        stop: CancellationToken,
    ) -> Result<Reply> {
        self.run(stop,move|db|{
            let revision:i64=db.connection.query_row("SELECT revision FROM meta",[],|r|r.get(0)).map_err(sql)?;
            if revision<0{return Err(invalid("invalid content revision"));}
            let visibility=hex::encode(Sha256::digest(serde_json::to_vec(&sources.iter().map(|s|(&s.scope,&s.coverage.source,s.reference_allowed)).collect::<Vec<_>>()).map_err(invalid)?));
            let start=if let Some(cursor)=after {
                if cursor.generation!=db.generation || cursor.revision!=revision.to_string() || cursor.caller!=caller || cursor.visibility!=visibility {return Ok(Reply::Stale{reason:"History content or visibility changed; search again".into()});}
                i64::try_from(rsi_history_api::decimal(&cursor.after)?).map_err(invalid)?
            }else{0};
            let admitted:BTreeMap<_,_>=sources.into_iter().filter(|s|s.coverage.indexed_through!="0").map(|s|(key(&s.scope),s)).collect();
            if admitted.is_empty(){return Ok(Reply::Matches{progress,matches:vec![],next:None});}
            let lexical=query.split_whitespace().map(|w|format!("\"{}\"",w.replace('"',"\"\""))).collect::<Vec<_>>().join(" AND ");
            let keys=admitted.keys().map(|k|format!("\"{k}\"")).collect::<Vec<_>>().join(" OR ");
            let expression=format!("source : ({keys}) AND body : ({lexical})");
            let mut statement=db.connection.prepare("SELECT rowid,source,length(CAST(metadata AS BLOB)) FROM documents WHERE documents MATCH ?1 AND rowid>?2 ORDER BY rowid LIMIT 65").map_err(sql)?;
            let coordinates=statement.query_map(params![expression,start],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?))).map_err(sql)?.collect::<std::result::Result<Vec<_>,_>>().map_err(sql)?;
            let mut more=coordinates.len()>64;
            let mut retained=32*1024;
            let mut selected=Vec::new();
            for (id,source,length) in coordinates.into_iter().take(64) {
                let length=usize::try_from(length).map_err(invalid)?;
                if id<=0 || length==0 || length>16384 || !admitted.contains_key(&source){return Err(invalid("invalid query candidate"));}
                let charge=length+serde_json::to_vec(&admitted[&source].scope).map_err(invalid)?.len()+serde_json::to_vec(&admitted[&source].label).map_err(invalid)?.len()+128;
                if retained+charge>256*1024 {more=true;break;}
                retained+=charge;selected.push((id,source));
            }
            let mut matches=Vec::new();
            let ids=selected.iter().map(|(id,_)|id.to_string()).collect::<Vec<_>>().join(",");
            let metadata=if selected.is_empty(){BTreeMap::new()}else{
                let mut rows=db.connection.prepare(&format!("SELECT rowid,metadata FROM documents WHERE rowid IN ({ids}) ORDER BY rowid")).map_err(sql)?;
                rows.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).map_err(sql)?.collect::<std::result::Result<BTreeMap<_,_>,_>>().map_err(sql)?
            };
            for (id,source) in &selected {
                let bytes=metadata.get(id).ok_or_else(||invalid("query metadata missing"))?;
                let hit:Hit=serde_json::from_str(bytes).map_err(invalid)?;hit.validate()?;
                let source=&admitted[source];
                if hit.source!=source.coverage.source {return Err(invalid("cached source identity mismatch"));}
                matches.push(Match{label:source.label.clone(),scope:source.scope.clone(),hit,reference_allowed:source.reference_allowed});
            }
            let next=if more {Some(QueryCursor{generation:db.generation.clone(),revision:revision.to_string(),query,scope:range,caller,visibility,after:selected.last().ok_or_else(||invalid("empty query continuation"))?.0.to_string()})}else{None};
            Ok(Reply::Matches{progress,matches,next})
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn maximum_catalog_query_keeps_admitted_hits_and_excludes_other_sources() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache")).await.unwrap();
        let mut sources = Vec::new();
        for index in 0..4096 {
            let id = format!("saved-{index}");
            let scope: Scope = serde_json::from_value(json!({"workspace":format!("{index:064x}"),"conversation":{"kind":"external","id":id}})).unwrap();
            let source = ReferenceSource::Observed {
                owner: "acp".into(),
                id,
                epoch: 1,
            };
            sources.push(SourceCoverage {
                label: "saved".into(),
                scope,
                unavailable: false,
                reference_allowed: true,
                coverage: Coverage {
                    source,
                    indexed_through: "1".into(),
                    observed_through: "1".into(),
                    omissions: "0".into(),
                    has_more: false,
                },
            });
        }
        let selected = sources[2048].clone();
        cache.run(CancellationToken::new(), move |db| {
            let hit: Hit = serde_json::from_value(json!({"source":selected.coverage.source,"original":{"record":{"sequence":"1","kind":"human","content_index":0},"through_seq":"1","start":0,"end":6,"text_sha256":"a".repeat(64),"scanned_bytes":128},"preview":"needle"})).unwrap();
            db.connection.execute("INSERT INTO documents(body,source,metadata) VALUES('needle','denied','{}')",[]).map_err(sql)?;
            db.connection.execute("INSERT INTO documents(body,source,metadata) VALUES('needle',?1,?2)", params![key(&selected.scope),serde_json::to_string(&hit).unwrap()]).map_err(sql)?;
            Ok(())
        }).await.unwrap();
        let Reply::Matches { matches, next, .. } = cache
            .query(
                QueryScope::AccessibleHost,
                "needle".into(),
                None,
                "a".repeat(64),
                sources,
                QueryProgress::default(),
                CancellationToken::new(),
            )
            .await
            .unwrap()
        else {
            panic!("maximum catalog page")
        };
        assert_eq!(matches.len(), 1);
        assert_eq!(
            serde_json::to_value(&matches[0].scope.conversation).unwrap()["id"],
            "saved-2048"
        );
        assert!(next.is_none());
        cache.close().await;
    }

    #[tokio::test]
    async fn global_query_pages_only_admitted_sources_and_fences_revision_caller_and_visibility() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache")).await.unwrap();
        let mut sources = Vec::new();
        for (index, id) in ["first", "second", "denied"].into_iter().enumerate() {
            let scope:Scope=serde_json::from_value(json!({"workspace":format!("{:064x}",index+1),"conversation":{"kind":"external","id":id}})).unwrap();
            cache
                .remember(scope.clone(), CancellationToken::new())
                .await
                .unwrap();
            let source = ReferenceSource::Observed {
                owner: "acp".into(),
                id: id.into(),
                epoch: 1,
            };
            let coverage = Coverage {
                source: source.clone(),
                indexed_through: "70".into(),
                observed_through: "70".into(),
                omissions: "0".into(),
                has_more: false,
            };
            let source_key = key(&scope);
            cache.run(CancellationToken::new(),move|db|{
                for sequence in 1..=70 {
                    let hit:Hit=serde_json::from_value(json!({"source":source,"original":{"record":{"sequence":sequence.to_string(),"kind":"human","content_index":0},"through_seq":"70","start":0,"end":6,"text_sha256":"a".repeat(64),"scanned_bytes":128},"preview":"needle"})).unwrap();
                    db.connection.execute("INSERT INTO documents(body,source,metadata) VALUES('needle',?1,?2)",params![source_key,serde_json::to_string(&hit).unwrap()]).map_err(sql)?;
                }
                Ok(())
            }).await.unwrap();
            if id != "denied" {
                sources.push(SourceCoverage {
                    label: id.into(),
                    unavailable: false,
                    scope,
                    coverage,
                    reference_allowed: true,
                });
            }
        }
        let query = |after, caller, sources: Vec<SourceCoverage>| {
            cache.query(
                QueryScope::AccessibleHost,
                "needle".into(),
                after,
                caller,
                sources,
                QueryProgress::default(),
                CancellationToken::new(),
            )
        };
        let caller = "a".repeat(64);
        let Reply::Matches {
            matches,
            next: Some(cursor),
            ..
        } = query(None, caller.clone(), sources.clone()).await.unwrap()
        else {
            panic!("first bounded page")
        };
        assert_eq!(matches.len(), 64);
        assert!(matches.iter().all(|m| m.label == "first"));
        let Reply::Matches { matches, .. } =
            query(Some(cursor.clone()), caller.clone(), sources.clone())
                .await
                .unwrap()
        else {
            panic!("stable next page")
        };
        assert_eq!(matches.len(), 64);
        assert!(matches.iter().all(|m| m.label != "denied"));
        assert!(matches!(
            query(Some(cursor.clone()), "b".repeat(64), sources.clone())
                .await
                .unwrap(),
            Reply::Stale { .. }
        ));
        assert!(matches!(
            query(Some(cursor.clone()), caller.clone(), sources[..1].to_vec())
                .await
                .unwrap(),
            Reply::Stale { .. }
        ));
        cache
            .rebuild(
                key(&sources[0].scope),
                sources[0].coverage.source.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            query(Some(cursor), caller, sources).await.unwrap(),
            Reply::Stale { .. }
        ));
        cache.close().await;
    }
}

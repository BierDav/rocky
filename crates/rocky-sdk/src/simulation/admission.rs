use super::cluster::{Member, SCHEMA};
use crate::{AwaitingAdmission, Failure, JoinRefused, transport::memory::Hub};

use std::{sync::Arc, time::Duration};

fn writer() -> crate::GrantProfile {
    crate::GrantProfile::Writer {
        scopes: ["items".into()].into(),
    }
}
async fn events(
    issuer: &Member,
    matches: impl Fn(&crate::ActivityKind) -> bool,
) -> Result<Vec<crate::ActivityEvent>, Failure> {
    Ok(issuer
        .session()?
        .activity(0)
        .await?
        .events
        .into_iter()
        .filter(|event| matches(&event.detail))
        .collect())
}
async fn pending(issuer: &Member) -> Result<Vec<crate::JoinRequest>, Failure> {
    Ok(issuer.session()?.discovered().await?.pending)
}
async fn awaiting(device: &mut Member, hub: &Arc<Hub>, uri: &str) -> Result<String, Failure> {
    let error = device
        .join(hub, uri)
        .await
        .err()
        .ok_or("joined unadmitted")?;
    Ok(error
        .downcast_ref::<AwaitingAdmission>()
        .ok_or_else(|| format!("not awaiting admission: {error}"))?
        .code
        .clone())
}

#[tokio::test(start_paused = true)]
async fn a_requested_join_waits_for_admission_with_the_code_both_devices_show()
-> Result<(), Failure> {
    let _seeded = rocky_platform::entropy::simulation::seed(51);
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let mut issuer = Member::new(root.path(), "issuer", SCHEMA);
            issuer.create(&hub).await?;
            let invitation = issuer.session()?.invitation().await?;
            assert_eq!(invitation.profile, None);
            let uri = invitation.uri;

            let mut device = Member::new(root.path(), "laptop", SCHEMA);
            let code = awaiting(&mut device, &hub, &uri).await?;
            assert_eq!(
                awaiting(&mut device, &hub, &uri).await?,
                code,
                "a retry keeps its request and code"
            );
            let listed = pending(&issuer).await?;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "laptop");
            assert_eq!(listed[0].code, code.as_str());
            assert_eq!(listed[0].invitation, invitation.id);
            let node = listed[0].node;
            assert_eq!(
                events(&issuer, |event| matches!(
                    event,
                    crate::ActivityKind::JoinRequested { .. }
                ))
                .await?
                .len(),
                1
            );

            let wrong = format!("{:06}", (code.parse::<u32>()? + 1) % 1_000_000);
            let admit = |code: &str| crate::Admit {
                node,
                code: code.into(),
                profile: writer(),
                label: None,
            };
            assert!(
                issuer.session()?.admit(admit(&wrong)).await.is_err(),
                "a wrong code is refused"
            );
            assert_eq!(
                pending(&issuer).await?.len(),
                1,
                "a refused code leaves the request"
            );
            assert_eq!(awaiting(&mut device, &hub, &uri).await?, code);

            issuer.close().await?;
            issuer.open(&hub).await?;
            assert_eq!(
                pending(&issuer).await?,
                listed,
                "pending requests survive issuer restarts"
            );
            issuer.session()?.admit(admit(&code)).await?;
            assert!(pending(&issuer).await?.is_empty());
            issuer.close().await?;
            issuer.open(&hub).await?;
            device.join(&hub, &uri).await?;
            let session = device.session()?.info().await?;
            assert_eq!(session.node, node);
            assert_eq!(
                events(&issuer, |event| matches!(
                    event,
                    crate::ActivityKind::IdentityAdmitted { .. }
                ))
                .await?
                .len(),
                1
            );

            let mut refused = Member::new(root.path(), "phone", SCHEMA);
            awaiting(&mut refused, &hub, &uri).await?;
            let node = pending(&issuer).await?[0].node;
            issuer.session()?.refuse(node).await?;
            let error = refused
                .join(&hub, &uri)
                .await
                .err()
                .ok_or("joined refused")?;
            assert!(
                error.downcast_ref::<JoinRefused>().is_some(),
                "a refused device learns it at its next join: {error}"
            );
            assert_eq!(
                events(&issuer, |event| matches!(
                    event,
                    crate::ActivityKind::JoinRefused { .. }
                ))
                .await?
                .len(),
                1
            );

            let mut late = Member::new(root.path(), "tablet", SCHEMA);
            awaiting(&mut late, &hub, &uri).await?;
            rocky_platform::entropy::simulation::pass(Duration::from_secs(11 * 60));
            assert!(
                pending(&issuer).await?.is_empty(),
                "an expired invitation ends its request"
            );
            let dropped = events(&issuer, |event| {
                matches!(event, crate::ActivityKind::JoinRequestDropped { .. })
            })
            .await?;
            let reasons: Vec<_> = dropped
                .iter()
                .filter_map(|event| match &event.detail {
                    crate::ActivityKind::JoinRequestDropped {
                        request, reason, ..
                    } => Some((request.name.as_str(), *reason)),
                    _ => None,
                })
                .collect();
            assert!(
                reasons.contains(&("tablet", crate::JoinRequestDropReason::Expired)),
                "{dropped:?}"
            );
            assert!(late.join(&hub, &uri).await.is_err());
            Ok(())
        })
        .await
}

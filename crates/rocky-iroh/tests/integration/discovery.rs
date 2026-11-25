use rocky_core::net::{Failure, Found, LOCAL_PATH, PathState, TOPIC_PATH};
use rocky_iroh::{Attachment, Discover, Options, Topic, iroh::EndpointAddr, testing::LookupServer};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(20);

fn options(lookup: Option<&LookupServer>, local: bool) -> Options {
    Options {
        lookup: Some(lookup.map_or("disabled".into(), |server| server.url.clone())),
        topic: lookup.is_some(),
        local,
        ..Options::isolated(if local { "0.0.0.0:0" } else { "127.0.0.1:0" })
    }
}
async fn found(transport: &Attachment, node: [u8; 32]) -> Result<Found, Failure> {
    let discovery = transport.discovery().ok_or("discovery")?;
    tokio::time::timeout(WAIT, async {
        loop {
            match discovery.found().await {
                Some(found) if found.claimed_node == node => return Ok(found),
                Some(_) => {}
                None => return Err("discovery closed".into()),
            }
        }
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_of_a_topic_find_each_other_over_mdns_and_others_do_not() -> Result<(), Failure> {
    let (topic, other) = ([41; 32], [42; 32]);
    let a = options(None, true).start([1; 32]).await?;
    let b = options(None, true).start([2; 32]).await?;
    let outsider = options(None, true).start([3; 32]).await?;
    let (a_port, b_port, c_port) = (a.transport(), b.transport(), outsider.transport());
    let discovery = |port: &Attachment| port.discovery().ok_or("discovery");
    assert_eq!(
        discovery(&a)?.paths(),
        [
            (LOCAL_PATH.to_owned(), PathState::On),
            (TOPIC_PATH.to_owned(), PathState::Off)
        ]
    );
    discovery(&a)?.announce([11; 32], &[topic]).await?;
    discovery(&b)?.announce([12; 32], &[topic]).await?;
    discovery(&outsider)?.announce([13; 32], &[other]).await?;
    let (at_a, at_b) = tokio::try_join!(found(&a, [12; 32]), found(&b, [11; 32]))?;
    assert!(at_a.local && at_b.local);
    assert_eq!(at_a.path, LOCAL_PATH);
    let address = endpoint(&at_a.address)?;
    assert_eq!(address.id, b.endpoint().id());
    assert!(
        address.ip_addrs().next().is_some(),
        "the found address is dialable as it is"
    );
    let silence = Duration::from_secs(2);
    assert!(
        tokio::time::timeout(silence, discovery(&outsider)?.found())
            .await
            .is_err(),
        "an endpoint of another topic hears of no member"
    );
    while let Ok(heard) =
        tokio::time::timeout(Duration::from_millis(10), discovery(&a)?.found()).await
    {
        assert_eq!(
            heard.ok_or("closed")?.claimed_node,
            [12; 32],
            "members never hear of an endpoint of another topic"
        );
    }
    tokio::join!(a_port.close(), b_port.close(), c_port.close());
    assert!(discovery(&a)?.found().await.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_of_a_topic_find_each_other_through_the_rendezvous_record() -> Result<(), Failure> {
    let server = LookupServer::start().await?;
    let (topic, previous) = ([51; 32], [52; 32]);
    let a = options(Some(&server), false).start([4; 32]).await?;
    let b = options(Some(&server), false).start([5; 32]).await?;
    let (a_port, b_port) = (a.transport(), b.transport());
    let (a_finder, b_finder) = (
        a.discovery().ok_or("discovery")?,
        b.discovery().ok_or("discovery")?,
    );
    assert_eq!(a_finder.paths()[1], (TOPIC_PATH.to_owned(), PathState::On));
    a_finder.announce([14; 32], &[topic, previous]).await?;
    tokio::time::timeout(WAIT, server.wait_for_contacts(&previous, 1)).await?;
    let only_the_previous_topic = [previous];
    b_finder
        .announce([15; 32], &only_the_previous_topic)
        .await?;
    let at_b = found(&b, [14; 32]).await?;
    assert!(!at_b.local);
    assert_eq!(at_b.path, TOPIC_PATH);
    let address = endpoint(&at_b.address)?;
    assert_eq!(address, a.endpoint().addr());
    tokio::time::timeout(WAIT, server.wait_for_contacts(&previous, 2)).await?;
    assert_eq!(server.contacts(&topic), 1);
    assert!(
        server
            .keys()
            .contains(&rocky_iroh::rendezvous_key(&topic).public()),
        "each topic's record is stored under a key only members derive"
    );
    let stored = server.stored();
    for node in [[14u8; 32], [15; 32]] {
        assert!(
            !stored.windows(32).any(|window| window == node),
            "the lookup service holds no node key in the clear"
        );
    }
    tokio::join!(a_port.close(), b_port.close());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attached_endpoint_keeps_its_lookup_services_and_discovers_only_when_asked()
-> Result<(), Failure> {
    let server = LookupServer::start().await?;
    let endpoint = options(Some(&server), false).bind([6; 32]).await?;
    let services = endpoint.address_lookup()?.len();
    assert!(services > 0);
    assert!(
        Attachment::new(endpoint.clone()).discovery().is_none(),
        "an attachment discovers only when asked"
    );
    let attachment = Attachment::discovering(
        endpoint.clone(),
        Discover {
            topic: Topic::On(server.url.clone()),
            local: true,
        },
    );
    let port = attachment.transport();
    let discovery = attachment.discovery().ok_or("discovery")?;
    discovery.announce([16; 32], &[[61; 32]]).await?;
    tokio::time::timeout(WAIT, server.wait_for_contacts(&[61; 32], 1)).await?;
    assert_eq!(endpoint.address_lookup()?.len(), services);
    discovery.announce([16; 32], &[]).await?;
    port.close().await;
    assert!(
        !endpoint.is_closed(),
        "closing Rocky's part leaves the endpoint open"
    );
    assert_eq!(
        endpoint.address_lookup()?.len(),
        services,
        "closing Rocky's part leaves the endpoint's lookup services"
    );
    endpoint.close().await;
    Ok(())
}

#[test]
fn a_disabled_lookup_service_makes_the_topic_unavailable_with_its_reason() {
    let asked = Options {
        topic: true,
        ..Options::isolated("127.0.0.1:0")
    }
    .discover();
    assert!(matches!(asked.topic, Topic::Unavailable(reason) if reason.contains("lookup")));
    assert_eq!(
        Options::isolated("127.0.0.1:0").discover(),
        Discover::default()
    );
}

fn endpoint(route: &str) -> Result<EndpointAddr, Failure> {
    let encoded = route.strip_prefix("iroh:").ok_or("not an iroh: route")?;
    Ok(serde_json::from_slice(
        &data_encoding::BASE64URL_NOPAD.decode(encoded.as_bytes())?,
    )?)
}

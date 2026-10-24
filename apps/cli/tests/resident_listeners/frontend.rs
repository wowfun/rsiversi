use super::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in local Node integration; set RSI_TEST_NODE to an absolute executable"]
#[allow(clippy::too_many_lines)] // Real frontend admission, ticket redemption and owned child cleanup share this fixture.
async fn ordinary_product_watched_smoke() {
    let node = std::path::PathBuf::from(std::env::var_os("RSI_TEST_NODE").expect("RSI_TEST_NODE"));
    assert!(node.is_absolute() && node.is_file());
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    let root = fixture.temporary.path();
    std::fs::write(
        root.join("bridge.json"),
        include_str!("../../../../crates/rsi/desktop-bridge/wire-contract.json"),
    )
    .unwrap();
    std::fs::write(
        root.join("build.mjs"),
        "if(process.env.DEEPSEEK_API_KEY)process.exit(8);",
    )
    .unwrap();
    std::fs::write(root.join("frontend.mjs"), r"
import http from 'node:http';
import readline from 'node:readline';
import fs from 'node:fs';
if(process.env.DEEPSEEK_API_KEY)process.exit(8);
const desktopBridge=JSON.parse(fs.readFileSync('bridge.json','utf8'));
let id, upstream, protocol, stage=0, noisy=false;
const server=http.createServer((request,response)=>{
  if(!noisy){noisy=true;process.stdout.write('post-ready plugin diagnostic\n');}
  const outgoing=http.request(new URL(request.url,upstream),{method:request.method,headers:request.headers},incoming=>{
    response.writeHead(incoming.statusCode,incoming.headers);incoming.pipe(response);
  }); outgoing.on('error',()=>response.writeHead(502).end());request.pipe(outgoing);
});
const send=(message,suffix='')=>process.stdout.write(JSON.stringify({version:1,interface:protocol,run_id:id,message})+'\n'+suffix);
const lines=readline.createInterface({input:process.stdin});
lines.on('line',line=>{
  const frame=JSON.parse(line),message=frame.message; protocol=frame.interface;
  if(stage===0&&message.event==='start_frontend'){
    id=frame.run_id;stage=1;server.listen(message.port,'127.0.0.1',()=>{
      send({event:'frontend_bound',origin:`http://127.0.0.1:${server.address().port}`,desktop_bridge:desktopBridge});
    });
  }else if(stage===1&&frame.run_id===id&&message.event==='configure_proxy'){
    upstream=message.upstream;stage=2;
    fs.writeFileSync('ready.json',JSON.stringify({pid:process.pid,upstream}));
    send({event:'proxy_ready'},'same-chunk plugin diagnostic\n');
  }else process.exit(9);
});
const timer=setInterval(()=>{if(fs.existsSync('exit-frontend')){server.close();lines.close();process.stdin.destroy();clearInterval(timer);}},25);
lines.on('close',()=>{server.close();clearInterval(timer);});
").unwrap();
    let config = root.join("config/rsi/rsi.toml");
    let mut launch = rsi::LaunchConfiguration::parse(&std::fs::read(&config).unwrap()).unwrap();
    launch.sources.insert(
        "web".into(),
        rsi::LaunchSource {
            directory: root.into(),
            command: vec![node.to_str().unwrap().into(), "build.mjs".into()],
            output: "package/package.json".into(),
            inputs: vec![
                "build.mjs".into(),
                "frontend.mjs".into(),
                "bridge.json".into(),
            ],
            frontend: Some(rsi::LaunchFrontend {
                command: vec![node.to_str().unwrap().into(), "frontend.mjs".into()],
                port: 0,
            }),
        },
    );
    std::fs::write(&config, toml::to_string(&launch).unwrap()).unwrap();
    fixture.assert_success(&["host", "start"]);
    let (persistent, _) = web(&fixture, true).await;
    let mut child = fixture
        .tokio_command()
        .args(["--watch", "web", "--no-open"])
        .env("DEEPSEEK_API_KEY", "fixture-never-forward")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (origin, ticket) = line
        .strip_prefix("rsi web: ")
        .unwrap()
        .split_once("#rsi-launch=")
        .unwrap();
    assert_ne!(origin, persistent);
    assert!(
        bootstrap(origin, &format!("X-Rsi-Launch-Ticket: {ticket}"))
            .await
            .starts_with("HTTP/1.1 200")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), child.wait())
            .await
            .is_err(),
        "post-ready stdout must not terminate the frontend attachment"
    );
    let ready: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("ready.json")).unwrap()).unwrap();
    let upstream = ready["upstream"].as_str().unwrap();
    assert!(
        !bootstrap(upstream, "").await.starts_with("HTTP/1.1 200"),
        "backend must require the frontend Origin"
    );
    std::fs::write(root.join("exit-frontend"), "exit").unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !status.success(),
        "unexpected frontend exit must fail the invocation"
    );
    assert!(
        tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect(upstream.strip_prefix("http://").unwrap())
            .await
            .is_err()
    );
    assert!(
        tokio::net::TcpStream::connect(persistent.strip_prefix("http://").unwrap())
            .await
            .is_ok()
    );
    fixture.assert_success(&["host", "status"]);
}

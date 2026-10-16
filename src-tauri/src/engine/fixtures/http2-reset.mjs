import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import {createSecureServer} from 'node:http2';

// Test-only loopback certificate expires in October 2036; never install this CA in a system trust store.
const total=Number(process.argv[1]),mode=process.argv[2],root=process.env.FETCHRAIL_H2_FIXTURE_DIR;
const prefix=Buffer.alloc(128*1024,0x5a),body=Buffer.alloc(64*1024,0x5a);
let resets=0,sessions=0,active=0,peak=0;
const requests=[];
const liveSessions=new Set(),resetGroups=[];
const server=createSecureServer({key:readFileSync(join(root,'localhost-key.pem')),cert:readFileSync(join(root,'localhost-cert.pem'))});
server.on('session',session=>{session.fixtureId=++sessions;liveSessions.add(session);session.on('close',()=>liveSessions.delete(session));session.on('error',()=>{});});
server.on('sessionError',()=>{});
server.on('stream',(stream,headers)=>{
 stream.on('error',()=>{});
 if(headers[':path']==='/stats'){stream.respond({':status':200});stream.end(JSON.stringify({resets,sessions,peak,requests,resetGroups}));return;}
 if(headers[':path']==='/reset'){
  const affected=[...liveSessions];resetGroups.push({active,sessions:affected.map(s=>s.fixtureId)});resets++;stream.respond({':status':200});stream.end('reset');
  setTimeout(()=>affected.forEach(session=>session.destroy(new Error('intentional shared-session failure'))),20);return;
 }
 if(headers[':method']==='HEAD'){stream.respond({':status':200,'content-length':total,etag:'"fixture"','accept-ranges':'bytes'});stream.end();return;}
 const range=/^bytes=(\d+)-(\d+)$/.exec(headers.range??'');assert(range);
 const start=Number(range[1]),end=Number(range[2]);assert(start<=end&&end<total);assert.equal(headers['if-range'],'"fixture"');
 requests.push({start,end,session:stream.session.fixtureId});console.log(JSON.stringify([start,end]));
 active++;peak=Math.max(peak,active);stream.once('close',()=>active--);
 stream.respond({':status':206,'content-type':'application/octet-stream','content-length':end-start+1,'content-range':`bytes ${start}-${end}/${total}`,etag:'"fixture"'});
 if(mode==='h2-reset-always'||!resets){stream.write(prefix.subarray(0,Math.min(prefix.length,end-start+1)));return;}
 let remaining=end-start+1;
 function pump(){while(!stream.destroyed&&remaining){const n=Math.min(remaining,body.length);remaining-=n;if(!stream.write(body.subarray(0,n))){stream.once('drain',pump);return;}}if(!stream.destroyed)stream.end();}
 pump();
});
server.listen(0,'127.0.0.1',()=>console.log(JSON.stringify({port:server.address().port})));

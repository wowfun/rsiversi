// Compare decoded screenshot channels in Chromium; no masks or golden refresh.
import {readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
export async function comparePixels(browser,report) {
 const page=await browser.newPage(),evidence=[];
 try{for(const width of [1440,1024])for(const theme of ['light','dark']){
  const urls=await Promise.all(['reference','rsi'].map(async variant=>'data:image/png;base64,'+(await readFile(join(report,`${variant}-${width}-${theme}.png`))).toString('base64')));
  const result=await page.evaluate(async urls=>{
   const images=await Promise.all(urls.map(async url=>{const image=new Image();image.src=url;await image.decode();return image}));
   const [expected,actual]=images;if(expected.width!==actual.width||expected.height!==actual.height)throw Error('Screenshot dimensions differ');
   const canvas=document.createElement('canvas');canvas.width=expected.width;canvas.height=expected.height;
   const context=canvas.getContext('2d',{willReadFrequently:true}),pixels=images.map(image=>{context.drawImage(image,0,0);return context.getImageData(0,0,canvas.width,canvas.height).data});
   const difference=context.createImageData(canvas.width,canvas.height);let count=0;
   for(let at=0;at<pixels[0].length;at+=4){let changed=false;for(let c=0;c<3;c++){const d=Math.abs(pixels[0][at+c]-pixels[1][at+c]);difference.data[at+c]=d;changed ||= d>16}difference.data[at+3]=255;if(changed)count++}
   context.putImageData(difference,0,0);return {differentPixels:count,fraction:count/(canvas.width*canvas.height),image:canvas.toDataURL().split(',')[1]};
  },urls);
  const {image,...measurement}=result;await writeFile(join(report,`difference-${width}-${theme}.png`),Buffer.from(image,'base64'));evidence.push({width,theme,...measurement});
 }}finally{await page.close()}
 await writeFile(join(report,'pixels.json'),JSON.stringify(evidence,null,2));assert(evidence.every(item=>item.fraction<=.01),JSON.stringify(evidence));
}

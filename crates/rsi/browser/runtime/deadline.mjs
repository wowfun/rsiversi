export function deadline(milliseconds, now=()=>performance.now()) {
  const end=now()+milliseconds;
  return maximum=>{
    const remaining=Math.floor(end-now());
    if(remaining<=0){const error=new Error('Operation deadline elapsed');error.name='TimeoutError';throw error;}
    return Math.min(maximum,remaining);
  };
}

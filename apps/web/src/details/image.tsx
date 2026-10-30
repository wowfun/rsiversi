import { useEffect, useState } from "react";
import type {ImageData, ImageInterest} from './model.ts';
type Status = {message: string; url?: string; retry?: boolean};
export function ImageDetail({ data }: {data: ImageData}) {
  const [revision, retry] = useState(0),
    [status, setStatus] = useState<Status>({ message: "Loading image…" });
  useEffect(() => {
    let live = true,
      interest: ImageInterest | undefined;
    setStatus({ message: "Loading image…" });
    try {
      interest = data.acquire();
      Promise.resolve(interest.result).then(
        (url) => {
          if (live)
            setStatus(
              url ? { url, message: "" } : { message: "Image unavailable" },
            );
        },
        (error) => {
          if (live)
            setStatus({
              message: `Image unavailable: ${error instanceof Error ? error.message : String(error)}`,
              retry: true,
            });
        },
      );
    } catch (error) {
      setStatus({
        message: `Image unavailable: ${error instanceof Error ? error.message : String(error)}`,
        retry: true,
      });
    }
    return () => {
      live = false;
      interest?.release();
    };
  }, [data.owner, data.ticket, revision]);
  return (
    <div className="image-detail">
      <p className="hint">
        {data.media.width} × {data.media.height} · {data.media.bytes} bytes
      </p>
      {status.message && (
        <p className="hint image-preview-status">
          {status.message}{" "}
          {status.retry && (
            <button type="button" onClick={() => retry((value) => value + 1)}>
              Retry image
            </button>
          )}
        </p>
      )}
      {status.url && (
        <img
          className="image-preview"
          src={status.url}
          alt={`Image ${data.media.width} × ${data.media.height}`}
          onError={() =>
            setStatus({ message: "Image could not be displayed", retry: true })
          }
        />
      )}
    </div>
  );
}

import { useState, useEffect, useRef } from 'react';

const Timer = ({ timeLimit, onTimeUp, isActive, reset }) => {
  const [timeLeft, setTimeLeft] = useState(timeLimit);
  const endTimeRef = useRef(null);
  const onTimeUpRef = useRef(onTimeUp);

  useEffect(() => {
    onTimeUpRef.current = onTimeUp;
  }, [onTimeUp]);

  // Initialize or reset end time
  useEffect(() => {
    if (reset || !endTimeRef.current) {
      const newEndTime = Date.now() + timeLimit * 1000;
      endTimeRef.current = newEndTime;
      setTimeLeft(timeLimit);
    }
  }, [timeLimit, reset]);

  // Countdown using actual time difference
  useEffect(() => {
    if (!isActive || !endTimeRef.current) return;

    const interval = setInterval(() => {
      const now = Date.now();
      const remaining = Math.max(0, Math.floor((endTimeRef.current - now) / 1000));
      setTimeLeft(remaining);

      if (remaining === 0) {
        onTimeUpRef.current();
        const nextEndTime = now + timeLimit * 1000;
        endTimeRef.current = nextEndTime;
        setTimeLeft(timeLimit);
      }
    }, 1000);

    return () => clearInterval(interval);
  }, [isActive, timeLimit]);

  const formatTime = (seconds) => {
    const mins = Math.floor(seconds / 60);
    const secs = seconds % 60;
    return `${mins}:${secs.toString().padStart(2, '0')}`;
  };

  return (
    <div className="timer">
      {timeLimit && <span>{formatTime(timeLeft)}</span>}
    </div>
  );
};

export default Timer;

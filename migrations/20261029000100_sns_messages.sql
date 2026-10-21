-- SHOP-8: SNS notifications already applied, so a replayed or redelivered message changes
-- nothing. Kept for a day (maintenance.cleanup); messages over an hour old are refused anyway.
CREATE TABLE platform.sns_messages (
    topic_arn   text NOT NULL CHECK (length(topic_arn) <= 256),
    message_id  text NOT NULL CHECK (length(message_id) BETWEEN 1 AND 100),
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (topic_arn, message_id)
);

#!/bin/sh
# A stand-in for whatever a deploy message should run. It receives the message
# on stdin and its topic and id in APERIO_MESSAGE_TOPIC / APERIO_MESSAGE_ID.
set -e
echo "deploying from $(cat)"
echo "topic=$APERIO_MESSAGE_TOPIC id=$APERIO_MESSAGE_ID"

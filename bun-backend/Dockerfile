FROM oven/bun:1.3

WORKDIR /app

# Install ffmpeg for timeline video transcoding.
RUN apt-get update \
  && apt-get install -y --no-install-recommends ffmpeg \
  && rm -rf /var/lib/apt/lists/*

# Copy package files
COPY package.json bun.lock ./

# Install dependencies
RUN bun install --frozen-lockfile

# Copy app source
COPY . .

# Generate Prisma client from the final copied schema/source state.
RUN bun run db:generate

# Expose port (DO injects PORT)
EXPOSE 8080

# Start server
COPY start.sh /start.sh
RUN chmod +x /start.sh

CMD ["/start.sh"]

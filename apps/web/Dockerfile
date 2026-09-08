# Build the bundle, then serve it from nginx on 8080. k8s/site.yaml relies on exactly two
# things from this image: it listens on 8080 and answers /healthz.
FROM node:22-alpine AS build
WORKDIR /app
# Copied ahead of the sources so the install layer is reused when only app code changes.
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM nginx:1.27-alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 8080
